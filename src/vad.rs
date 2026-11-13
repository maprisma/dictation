//! A simple energy voice activity detector, driven by audio time (not the clock), so it behaves
//! the same live and in tests.
//!
//! In the first 250 ms quiet chunks (below 0.05) only teach the noise floor. A chunk is speech when its RMS is above `max(min_threshold, noise floor × ratio)`; the noise
//! floor follows the quiet chunks. Speech counts as started after `min_speech_ms` of speech in a
//! row. With `end = silence`: finish after `silence_ms` of quiet once speech was heard, cancel
//! after `initial_ms` without speech. Both modes stop at `max_ms`.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VadConfig {
    pub silence_ms: u32,
    pub initial_ms: u32,
    pub max_ms: u32,
    pub min_speech_ms: u32,
    pub min_threshold: f64,
    pub ratio: f64,
    /// The first ms: chunks below `calibration_ceiling` only teach the noise floor.
    pub calibration_ms: u32,
    pub calibration_ceiling: f64,
}

impl Default for VadConfig {
    fn default() -> Self {
        VadConfig { silence_ms: 900, initial_ms: 4000, max_ms: 60_000, min_speech_ms: 150, min_threshold: 0.012, ratio: 3.0, calibration_ms: 250, calibration_ceiling: 0.05 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Continue,
    /// Speech, then enough silence.
    Finish,
    /// No speech at the start for too long.
    NoSpeech,
    /// The hard cap.
    MaxLength,
}

#[derive(Debug, Clone)]
pub struct Vad {
    cfg: VadConfig,
    /// Auto-finish on silence (`end = silence`); otherwise only the cap applies.
    silence_end: bool,
    elapsed_ms: u32,
    speech_run_ms: u32,
    silence_run_ms: u32,
    heard: bool,
    noise: Option<f64>,
}

impl Vad {
    pub fn new(cfg: VadConfig, silence_end: bool) -> Self {
        Vad { cfg, silence_end, elapsed_ms: 0, speech_run_ms: 0, silence_run_ms: 0, heard: false, noise: None }
    }

    pub fn heard_speech(&self) -> bool {
        self.heard
    }

    pub fn elapsed_ms(&self) -> u32 {
        self.elapsed_ms
    }

    pub fn threshold(&self) -> f64 {
        self.cfg.min_threshold.max(self.noise.unwrap_or(0.0) * self.cfg.ratio)
    }

    /// One chunk's level and length.
    pub fn feed(&mut self, rms: f64, ms: u32) -> Decision {
        self.elapsed_ms += ms;
        // A noisy room above min_threshold would otherwise count as speech from the start and
        // the floor would never learn it.
        let calibrating = self.elapsed_ms <= self.cfg.calibration_ms && rms < self.cfg.calibration_ceiling;
        let speech = !calibrating && rms > self.threshold();
        if speech {
            self.speech_run_ms += ms;
            self.silence_run_ms = 0;
            if self.speech_run_ms >= self.cfg.min_speech_ms {
                self.heard = true;
            }
        } else {
            self.speech_run_ms = 0;
            self.silence_run_ms += ms;
            // The floor learns only from quiet chunks, slowly, and never from loud ones.
            self.noise = Some(match self.noise {
                None => rms,
                Some(n) if calibrating => n.max(rms),
                Some(n) => n * 0.95 + rms.min(0.05) * 0.05,
            });
        }
        if self.elapsed_ms >= self.cfg.max_ms {
            return Decision::MaxLength;
        }
        if self.silence_end {
            if self.heard && self.silence_run_ms >= self.cfg.silence_ms {
                return Decision::Finish;
            }
            if !self.heard && self.elapsed_ms >= self.cfg.initial_ms {
                return Decision::NoSpeech;
            }
        }
        Decision::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{CHUNK_MS, CHUNK_SAMPLES, rms, synth};

    /// Feeds PCM chunk by chunk; returns the decision and when (ms) it came.
    fn run(vad: &mut Vad, pcm: &[i16]) -> (Decision, u32) {
        for chunk in pcm.chunks(CHUNK_SAMPLES) {
            let d = vad.feed(rms(chunk), CHUNK_MS);
            if d != Decision::Continue {
                return (d, vad.elapsed_ms());
            }
        }
        (Decision::Continue, vad.elapsed_ms())
    }

    fn concat(parts: &[Vec<i16>]) -> Vec<i16> {
        parts.concat()
    }

    #[test]
    fn finishes_after_silence_following_speech() {
        let pcm = concat(&[synth::noise(500, 60), synth::tone(1200, 220.0, 6000.0), synth::noise(3000, 60)]);
        let mut vad = Vad::new(VadConfig::default(), true);
        let (d, at) = run(&mut vad, &pcm);
        assert_eq!(d, Decision::Finish);
        // Speech ends at 1700 ms; 900 ms of quiet later.
        assert_eq!(at, 1700 + 900);
        assert!(vad.heard_speech());
    }

    #[test]
    fn short_pauses_do_not_finish() {
        let pcm = concat(&[synth::tone(600, 220.0, 6000.0), synth::silence(700), synth::tone(600, 220.0, 6000.0), synth::silence(1000)]);
        let (d, at) = run(&mut Vad::new(VadConfig::default(), true), &pcm);
        assert_eq!((d, at), (Decision::Finish, 1900 + 900));
    }

    #[test]
    fn cancels_without_speech() {
        let pcm = synth::noise(6000, 80);
        let (d, at) = run(&mut Vad::new(VadConfig::default(), true), &pcm);
        assert_eq!((d, at), (Decision::NoSpeech, 4000));
        // A click (one loud chunk) is not speech.
        let click = concat(&[synth::silence(1000), synth::tone(50, 300.0, 20000.0), synth::silence(4000)]);
        assert_eq!(run(&mut Vad::new(VadConfig::default(), true), &click).0, Decision::NoSpeech);
    }

    #[test]
    fn hard_cap_and_release_mode() {
        let pcm = synth::tone(61_000, 220.0, 6000.0);
        assert_eq!(run(&mut Vad::new(VadConfig::default(), true), &pcm), (Decision::MaxLength, 60_000));
        // Release mode never ends on silence, only at the cap.
        let quiet = synth::silence(61_000);
        assert_eq!(run(&mut Vad::new(VadConfig::default(), false), &quiet), (Decision::MaxLength, 60_000));
        let speech_then_quiet = concat(&[synth::tone(1000, 220.0, 6000.0), synth::silence(5000)]);
        assert_eq!(run(&mut Vad::new(VadConfig::default(), false), &speech_then_quiet).0, Decision::Continue);
    }

    #[test]
    fn noise_floor_adapts() {
        // A noisy room (rms ≈ 0.02) is not speech; speech well above it is.
        let room = synth::noise(1000, 1200);
        let mut vad = Vad::new(VadConfig::default(), true);
        assert_eq!(run(&mut vad, &room).0, Decision::Continue);
        assert!(!vad.heard_speech());
        assert!(vad.threshold() > 0.03);
        let (d, _) = run(&mut vad, &concat(&[synth::tone(500, 200.0, 12000.0), synth::noise(1000, 1200)]));
        assert_eq!(d, Decision::Finish);
    }
}
