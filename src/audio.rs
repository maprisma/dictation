//! 16 kHz mono signed 16-bit PCM: level (RMS), WAV in memory, and reading other WAV files
//! (any rate, mono or stereo, 16-bit) for `transcribe`.

pub const RATE: u32 = 16_000;
/// One chunk from the recorder: 50 ms, so `Level` comes about 20 times a second.
pub const CHUNK_SAMPLES: usize = (RATE as usize) / 20;
pub const CHUNK_MS: u32 = 50;

/// Root mean square of the samples, 0.0–1.0 (full scale).
pub fn rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum();
    (sum / samples.len() as f64).sqrt()
}

pub fn ms_of(samples: usize) -> u64 {
    samples as u64 * 1000 / RATE as u64
}

/// Little-endian bytes to samples (an odd last byte is dropped).
pub fn from_le_bytes(bytes: &[u8]) -> Vec<i16> {
    bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
}

/// A complete WAV file (16 kHz mono s16) for the samples.
pub fn wav_bytes(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Reads a 16-bit PCM WAV (any rate, any channel count) as 16 kHz mono.
pub fn read_wav(bytes: &[u8]) -> Result<Vec<i16>, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let mut pos = 12;
    let mut format: Option<(u16, u16, u32, u16)> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = &bytes[pos + 8..(pos + 8 + len).min(bytes.len())];
        if id == b"fmt " && body.len() >= 16 {
            let u16_at = |i: usize| u16::from_le_bytes([body[i], body[i + 1]]);
            let rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            format = Some((u16_at(0), u16_at(2), rate, u16_at(14)));
        } else if id == b"data" {
            let (tag, channels, rate, bits) = format.ok_or("WAV without a format chunk")?;
            // 1 = PCM, 0xFFFE = extensible (PCM inside for 16 bit).
            if (tag != 1 && tag != 0xFFFE) || bits != 16 || channels == 0 || rate == 0 {
                return Err("only 16-bit PCM WAV files are supported".into());
            }
            let samples = from_le_bytes(body);
            return Ok(resample(&downmix(&samples, channels as usize), rate, RATE));
        }
        pos += 8 + len + (len & 1);
    }
    Err("WAV without audio data".into())
}

pub fn downmix(samples: &[i16], channels: usize) -> Vec<i16> {
    if channels <= 1 {
        return samples.to_vec();
    }
    samples.chunks_exact(channels).map(|f| (f.iter().map(|&s| s as i32).sum::<i32>() / channels as i32) as i16).collect()
}

/// Linear interpolation; good enough for speech recognition.
pub fn resample(samples: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    let out_len = (samples.len() as u64 * to as u64 / from as u64) as usize;
    let step = from as f64 / to as f64;
    (0..out_len)
        .map(|i| {
            let x = i as f64 * step;
            let j = x.floor() as usize;
            let frac = x - j as f64;
            let a = samples[j.min(samples.len() - 1)] as f64;
            let b = samples[(j + 1).min(samples.len() - 1)] as f64;
            (a + (b - a) * frac).round() as i16
        })
        .collect()
}

/// Test signals.
pub mod synth {
    use super::RATE;

    pub fn silence(ms: u32) -> Vec<i16> {
        vec![0; (RATE * ms / 1000) as usize]
    }

    /// Low noise like a quiet room (deterministic).
    pub fn noise(ms: u32, amplitude: i16) -> Vec<i16> {
        let mut x: u32 = 12345;
        (0..(RATE * ms / 1000))
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (((x >> 16) as i32 % (2 * amplitude as i32 + 1)) - amplitude as i32) as i16
            })
            .collect()
    }

    /// A tone standing in for speech.
    pub fn tone(ms: u32, hz: f64, amplitude: f64) -> Vec<i16> {
        (0..(RATE * ms / 1000)).map(|i| ((i as f64 * hz * std::f64::consts::TAU / RATE as f64).sin() * amplitude) as i16).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(rms(&synth::silence(50)), 0.0);
        let full = rms(&synth::tone(100, 440.0, 32767.0));
        assert!((full - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01, "{full}");
        assert!(rms(&synth::noise(100, 100)) < 0.01);
        assert_eq!(CHUNK_SAMPLES, 800);
        assert_eq!(ms_of(16_000), 1000);
    }

    #[test]
    fn wav_round_trip() {
        let pcm = synth::tone(200, 300.0, 8000.0);
        let wav = wav_bytes(&pcm);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(wav.len(), 44 + pcm.len() * 2);
        assert_eq!(read_wav(&wav).unwrap(), pcm);
        assert!(read_wav(b"nope").is_err());
    }

    #[test]
    fn other_wav_formats() {
        // 48 kHz stereo -> 16 kHz mono.
        let mono = synth::tone(100, 200.0, 4000.0);
        let mut stereo48 = Vec::new();
        for &s in &resample(&mono, 16_000, 48_000) {
            stereo48.extend([s, s]);
        }
        let mut wav = wav_bytes(&stereo48);
        wav[22..24].copy_from_slice(&2u16.to_le_bytes());
        wav[24..28].copy_from_slice(&48_000u32.to_le_bytes());
        let back = read_wav(&wav).unwrap();
        assert!((back.len() as i64 - mono.len() as i64).abs() <= 1);
        assert!((rms(&back) - rms(&mono)).abs() < 0.005);
        // 8-bit is refused.
        let mut eight = wav_bytes(&mono);
        eight[34..36].copy_from_slice(&8u16.to_le_bytes());
        assert!(read_wav(&eight).is_err());
    }
}
