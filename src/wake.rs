//! The wake phrase ("Hey Linux"): while `wake_enabled` is on and nothing else uses the
//! microphone, the daemon listens, cuts short stretches of speech with an energy detector
//! ([Segmenter]), turns each one into text with a small local Whisper model on its own CPU-only
//! whisper-server (never an online or external engine), and compares it with the phrase
//! ([match_phrase]). On a match it sends `Wake(phrase, rest)` and starts
//! `maprisma-dictation --wake [--ask-rest REST]`.
//!
//! Privacy: audio stays in memory and each stretch is dropped right after it was compared;
//! nothing is logged but "wake phrase heard". While it listens the microphone is open (the bar
//! icon shows it).
//!
//! Sessions always win the microphone (core.rs [MicShare]): a `StartSession` makes the listener
//! close the microphone before the session opens it. The listener waits while a session runs
//! (and 1 s after it), while an answer is read aloud, and 3 s after a match.

use crate::VoiceError;
use crate::audio::{self, CHUNK_MS};
use crate::core::{MicShare, MicState};
use crate::engines::Engine;
use crate::models::ModelFile;
use crate::paths::Dirs;
use crate::recorder::Capture;
use crate::settings::Settings;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

// ------------------------------------------------------------------------- text

/// Folds the accents of Latin letters (`ä` → `a`, `ß` → `ss`, `ł` → `l` …).
fn fold(c: char, out: &mut String) {
    let plain = match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'č' => "c",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' => "e",
        'ì' | 'í' | 'î' | 'ï' | 'ī' => "i",
        'ñ' | 'ń' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => "o",
        'œ' => "oe",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' => "u",
        'ý' | 'ÿ' => "y",
        'ß' => "ss",
        'ł' => "l",
        'ś' | 'š' => "s",
        'ź' | 'ż' | 'ž' => "z",
        'ř' => "r",
        'ď' => "d",
        'ť' => "t",
        _ => {
            out.push(c);
            return;
        }
    };
    out.push_str(plain);
}

/// One word of a text: normalised, and where it ends in the original (bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    norm: String,
    end: usize,
}

/// Words are runs of letters and digits (an apostrophe inside one is dropped: "it's" → "its");
/// everything else separates them.
fn words(text: &str) -> Vec<Word> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut end = 0;
    for (i, c) in text.char_indices() {
        if c.is_alphanumeric() {
            for lower in c.to_lowercase() {
                fold(lower, &mut current);
            }
            end = i + c.len_utf8();
        } else if (c == '\'' || c == '’') && !current.is_empty() {
            // inside a word; it ends at the next letter or separator
        } else if !current.is_empty() {
            out.push(Word { norm: std::mem::take(&mut current), end });
        }
    }
    if !current.is_empty() {
        out.push(Word { norm: current, end });
    }
    out
}

/// Lowercase, accents folded, punctuation dropped, single spaces: "Hey, Linux!" → "hey linux".
pub fn normalize(text: &str) -> String {
    words(text).into_iter().map(|w| w.norm).collect::<Vec<_>>().join(" ")
}

/// The edit distance between two strings (in characters).
pub fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != *cb)).min(above + 1).min(row[j] + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

/// A light sound folding for the comparison: the "ai" sound's spellings (`igh`, `ey`, `ei`,
/// `ay`) become `ai` and a final `y` becomes `i`, so "High Linux" / "Hei Linux" match "Hey Linux".
pub fn sound(normalized: &str) -> String {
    normalized
        .split(' ')
        .map(|w| {
            let mut w = w.replace("igh", "ai").replace("ey", "ai").replace("ei", "ai").replace("ay", "ai");
            if w.len() > 1 && w.ends_with('y') {
                w.pop();
                w.push('i');
            }
            w
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The normalised edit distance (0 = the same, 1 = nothing alike) of two normalised texts: the
/// smaller of the letters' and the sounds' ([sound]).
pub fn distance(a: &str, b: &str) -> f64 {
    let d = |x: &str, y: &str| levenshtein(x, y) as f64 / x.chars().count().max(y.chars().count()).max(1) as f64;
    d(a, b).min(d(&sound(a), &sound(b)))
}

/// The most a match may differ from the phrase (normalised edit distance) per `wake_sensitivity`.
pub fn max_distance(sensitivity: &str) -> f64 {
    match sensitivity {
        "strict" => 0.15,
        "loose" => 0.35,
        _ => 0.25,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub matched: bool,
    /// 1 − the normalised edit distance of the best candidate (1 = exactly the phrase).
    pub score: f64,
    /// What came after the phrase, as heard (punctuation at its start dropped).
    pub rest: String,
}

/// Compares what was heard with the phrase ([distance] of the normalised words): the phrase must be at the start (one
/// word before it is allowed, "oh hey Linux"), and may be heard as one word more or less ("Hey
/// Linux" as "Heylinux"). The rest after it is the request ("Hey Linux, open Firefox").
pub fn match_phrase(phrase: &str, heard: &str, max_distance: f64) -> Match {
    let target = normalize(phrase);
    let n = target.split(' ').count();
    let heard_words = words(heard);
    let mut best: Option<(f64, usize, usize, usize)> = None; // (distance, skip, k, end)
    if !target.is_empty() {
        for skip in 0..=1usize.min(heard_words.len().saturating_sub(1)) {
            let available = heard_words.len() - skip;
            for k in n.saturating_sub(1).max(1)..=(n + 1).min(available) {
                let candidate = heard_words[skip..skip + k].iter().map(|w| w.norm.as_str()).collect::<Vec<_>>().join(" ");
                let distance = distance(&candidate, &target);
                // A skipped word or a different word count is a little worse than the same.
                let rank = (distance, skip, k.abs_diff(n), heard_words[skip + k - 1].end);
                if best.is_none_or(|b| (rank.0, rank.1, rank.2) < (b.0, b.1, b.2)) {
                    best = Some(rank);
                }
            }
        }
    }
    let Some((distance, _, _, end)) = best else { return Match { matched: false, score: 0.0, rest: String::new() } };
    let matched = distance <= max_distance;
    let rest = if matched { heard[end..].trim_start_matches(|c: char| c.is_whitespace() || ",.!?:;-–—…".contains(c)).trim_end().to_string() } else { String::new() };
    // Only punctuation left over is no request.
    let rest = if normalize(&rest).is_empty() { String::new() } else { rest.chars().take(MAX_REST).collect() };
    Match { matched, score: ((1.0 - distance) * 100.0).round() / 100.0, rest }
}

/// The longest request passed on to the overlay.
pub const MAX_REST: usize = 500;

// ------------------------------------------------------------------------- segments

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentConfig {
    /// Less speech than this in a stretch is not a phrase (a click, a cough).
    pub min_speech_ms: u32,
    /// A stretch is cut here: the first `max_ms` are checked, the rest is skipped until a pause.
    pub max_ms: u32,
    /// This much quiet ends a stretch.
    pub gap_ms: u32,
    /// Audio kept from just before the speech started (the first consonant is quiet).
    pub preroll_ms: u32,
    pub min_threshold: f64,
    pub ratio: f64,
    /// The first ms only teach the noise floor (chunks below `calibration_ceiling`).
    pub calibration_ms: u32,
    pub calibration_ceiling: f64,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        SegmentConfig { min_speech_ms: 400, max_ms: 3000, gap_ms: 600, preroll_ms: 300, min_threshold: 0.012, ratio: 3.0, calibration_ms: 300, calibration_ceiling: 0.05 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub pcm: Vec<i16>,
    /// It went on past `max_ms`: only its start is here (a request after the phrase is cut off).
    pub truncated: bool,
}

#[derive(Debug, Clone)]
enum Cut {
    Quiet,
    Speech { pcm: Vec<i16>, speech_ms: u32, span_ms: u32, silence_ms: u32 },
    /// After a cut-off stretch: wait for a pause.
    Skip { silence_ms: u32 },
}

/// Cuts a continuous stream into short stretches of speech, with the same energy rule as the
/// session VAD (vad.rs): speech is above `max(min_threshold, noise floor × ratio)`.
#[derive(Debug, Clone)]
pub struct Segmenter {
    cfg: SegmentConfig,
    noise: Option<f64>,
    elapsed_ms: u32,
    preroll: VecDeque<Vec<i16>>,
    cut: Cut,
}

impl Segmenter {
    pub fn new(cfg: SegmentConfig) -> Self {
        Segmenter { cfg, noise: None, elapsed_ms: 0, preroll: VecDeque::new(), cut: Cut::Quiet }
    }

    pub fn threshold(&self) -> f64 {
        self.cfg.min_threshold.max(self.noise.unwrap_or(0.0) * self.cfg.ratio)
    }

    fn learn(&mut self, rms: f64, calibrating: bool) {
        self.noise = Some(match self.noise {
            None => rms,
            Some(n) if calibrating => n.max(rms),
            Some(n) => n * 0.95 + rms.min(0.05) * 0.05,
        });
    }

    /// One chunk; a finished stretch of speech when this chunk ended one.
    pub fn feed(&mut self, chunk: &[i16]) -> Option<Segment> {
        let ms = (audio::ms_of(chunk.len()) as u32).clamp(1, CHUNK_MS * 4);
        let rms = audio::rms(chunk);
        self.elapsed_ms = self.elapsed_ms.saturating_add(ms);
        let calibrating = self.elapsed_ms <= self.cfg.calibration_ms && rms < self.cfg.calibration_ceiling;
        let speech = !calibrating && rms > self.threshold();
        if !speech {
            self.learn(rms, calibrating);
        }
        match &mut self.cut {
            Cut::Quiet => {
                if speech {
                    let mut pcm: Vec<i16> = self.preroll.drain(..).flatten().collect();
                    pcm.extend_from_slice(chunk);
                    self.cut = Cut::Speech { pcm, speech_ms: ms, span_ms: ms, silence_ms: 0 };
                } else {
                    self.preroll.push_back(chunk.to_vec());
                    while self.preroll.len() as u32 * CHUNK_MS > self.cfg.preroll_ms {
                        self.preroll.pop_front();
                    }
                }
                None
            }
            Cut::Speech { pcm, speech_ms, span_ms, silence_ms } => {
                pcm.extend_from_slice(chunk);
                *span_ms += ms;
                if speech {
                    *speech_ms += ms;
                    *silence_ms = 0;
                } else {
                    *silence_ms += ms;
                }
                let enough = *speech_ms >= self.cfg.min_speech_ms;
                if *silence_ms >= self.cfg.gap_ms {
                    let pcm = std::mem::take(pcm);
                    self.cut = Cut::Quiet;
                    return enough.then_some(Segment { pcm, truncated: false });
                }
                if *span_ms >= self.cfg.max_ms {
                    let pcm = std::mem::take(pcm);
                    let silence_ms = *silence_ms;
                    self.cut = Cut::Skip { silence_ms };
                    return enough.then_some(Segment { pcm, truncated: true });
                }
                None
            }
            Cut::Skip { silence_ms } => {
                *silence_ms = if speech { 0 } else { *silence_ms + ms };
                if *silence_ms >= self.cfg.gap_ms {
                    self.cut = Cut::Quiet;
                }
                None
            }
        }
    }
}

// ------------------------------------------------------------------------- the listener

/// What the listener needs from the daemon (the real one is [RealHost]; tests fake it).
pub trait WakeHost: Send + Sync + 'static {
    fn settings(&self) -> Settings;
    /// The local recogniser for these settings, or a `wake_why_not` code. Blocking.
    fn recogniser(&self, settings: &Settings) -> Result<Arc<dyn Engine>, &'static str>;
    /// Opens the microphone. Blocking.
    fn capture(&self, microphone: &str) -> Result<Capture, VoiceError>;
    /// An answer is being read aloud.
    fn speaking(&self) -> bool;
    /// Starts the overlay (`--wake`, and `--ask-rest REST` when REST isn't empty).
    fn launch(&self, rest: &str) -> std::io::Result<()>;
    /// Wake is off: free the recogniser's memory. Blocking.
    fn unload(&self) {}
}

#[derive(Debug, Clone, PartialEq)]
pub enum WakeEvent {
    /// The phrase was heard (the `Wake` signal).
    Heard { phrase: String, rest: String },
    /// The listener opened (true) or closed (false) the microphone (the `WakeListening` signal).
    Listening(bool),
}

#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// No listening for this long after a match.
    pub cooldown: Duration,
    /// … and this long after a session ended.
    pub after_session: Duration,
    /// How often settings and speaking are checked.
    pub tick: Duration,
    /// After the recorder failed or ended, wait this long before trying again.
    pub retry: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { cooldown: Duration::from_secs(3), after_session: Duration::from_secs(1), tick: Duration::from_millis(500), retry: Duration::from_secs(5) }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Shown {
    active: bool,
    why_not: &'static str,
}

/// What the listener listens with; a change of any of these restarts it.
#[derive(Debug, Clone, PartialEq)]
struct Setup {
    phrase: String,
    language: String,
    model: String,
    sensitivity: String,
    microphone: String,
    whisper_server: String,
}

impl Setup {
    fn of(s: &Settings) -> Self {
        Setup { phrase: s.wake_phrase.clone(), language: s.wake_language.clone(), model: s.wake_model.clone(), sensitivity: s.wake_sensitivity.clone(), microphone: s.microphone.clone(), whisper_server: s.whisper_server.clone() }
    }
}

enum Ended {
    /// A session wants the microphone, an answer is read aloud, the settings changed, shutdown.
    Paused,
    Heard(String),
    RecorderGone,
}

/// The result of `WakeTest` / `maprisma-voice wake test`.
#[derive(Debug, Clone, PartialEq)]
pub struct TestResult {
    pub heard: String,
    pub matched: bool,
    pub score: f64,
    /// The score a match needs (1 − the sensitivity's distance).
    pub needed: f64,
    pub rest: String,
    pub phrase: String,
    pub model: String,
    /// Speech was heard at all (false: only quiet within the time).
    pub speech: bool,
}

pub struct Wake {
    host: Arc<dyn WakeHost>,
    mic: Arc<MicShare>,
    events: mpsc::UnboundedSender<WakeEvent>,
    shown: Mutex<Shown>,
    cooldown_until: Mutex<Option<Instant>>,
    segments: SegmentConfig,
    timing: Timing,
    stop: tokio::sync::watch::Sender<bool>,
    /// One `WakeTest` at a time.
    testing: tokio::sync::Mutex<()>,
}

impl Wake {
    pub fn new(host: Arc<dyn WakeHost>, mic: Arc<MicShare>, segments: SegmentConfig, timing: Timing) -> (Arc<Wake>, mpsc::UnboundedReceiver<WakeEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let wake = Wake {
            host,
            mic,
            events: tx,
            shown: Mutex::new(Shown { active: false, why_not: "off" }),
            cooldown_until: Mutex::new(None),
            segments,
            timing,
            stop: tokio::sync::watch::channel(false).0,
            testing: tokio::sync::Mutex::new(()),
        };
        (Arc::new(wake), rx)
    }

    fn lock_shown(&self) -> std::sync::MutexGuard<'_, Shown> {
        self.shown.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `Status`'s wake keys.
    pub fn status(&self) -> serde_json::Map<String, serde_json::Value> {
        let s = self.host.settings();
        let shown = self.lock_shown().clone();
        let mut m = serde_json::Map::new();
        m.insert("wake_enabled".into(), s.wake_enabled.into());
        m.insert("wake_active".into(), shown.active.into());
        m.insert("wake_phrase".into(), s.wake_phrase.clone().into());
        m.insert("wake_why_not".into(), shown.why_not.into());
        m.insert("wake_message".into(), crate::i18n::tr(why_not_key(shown.why_not)).into());
        m
    }

    /// Ends the listener (the daemon stops); it closes the microphone.
    pub fn shutdown(&self) {
        self.stop.send_replace(true);
    }

    fn stopping(&self) -> bool {
        *self.stop.borrow()
    }

    fn show(&self, active: bool, why_not: &'static str) {
        let was = {
            let mut shown = self.lock_shown();
            let was = shown.active;
            *shown = Shown { active, why_not };
            was
        };
        if was != active {
            let _ = self.events.send(WakeEvent::Listening(active));
        }
    }

    /// Why the listener can't listen now ("" = it can), from what is cheap to know.
    fn why_waiting(&self, s: &Settings, mic: &MicState) -> &'static str {
        if !s.wake_enabled {
            return "off";
        }
        if !s.enabled {
            return "disabled";
        }
        if mic.sessions > 0 || mic.last_end.is_some_and(|t| t.elapsed() < self.timing.after_session) {
            return "busy";
        }
        if self.host.speaking() {
            return "speaking";
        }
        if self.cooldown_until.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| Instant::now() < t) {
            return "cooldown";
        }
        ""
    }

    /// The listener: runs until [Wake::shutdown].
    pub async fn run(self: Arc<Self>) {
        let mut mic_rx = self.mic.subscribe();
        let mut stop_rx = self.stop.subscribe();
        let mut unloaded = true;
        while !self.stopping() {
            let s = self.host.settings();
            let mut why = self.why_waiting(&s, &self.mic.state());
            let mut recogniser = None;
            if why.is_empty() {
                let host = self.host.clone();
                let settings = s.clone();
                match tokio::task::spawn_blocking(move || host.recogniser(&settings)).await {
                    Ok(Ok(r)) => recogniser = Some(r),
                    Ok(Err(code)) => why = code,
                    Err(_) => why = "failed",
                }
            }
            if matches!(why, "off" | "disabled") && !unloaded {
                let host = self.host.clone();
                let _ = tokio::task::spawn_blocking(move || host.unload()).await;
                unloaded = true;
            }
            let (Some(recogniser), true) = (recogniser, why.is_empty()) else {
                self.show(false, why);
                tokio::select! {
                    _ = mic_rx.changed() => {}
                    _ = stop_rx.changed() => {}
                    _ = tokio::time::sleep(self.timing.tick) => {}
                }
                continue;
            };
            if !self.mic.try_hold_wake() {
                // A session started just now.
                tokio::task::yield_now().await;
                continue;
            }
            let host = self.host.clone();
            let microphone = s.microphone.clone();
            let capture = match tokio::task::spawn_blocking(move || host.capture(&microphone)).await {
                Ok(Ok(c)) => c,
                _ => {
                    self.mic.release_wake();
                    self.show(false, "recorder_failed");
                    self.wait_or_stop(self.timing.retry).await;
                    continue;
                }
            };
            unloaded = false;
            // A session may have asked for the microphone while it opened.
            self.show(true, "");
            let ended = self.listen(capture, recogniser, Setup::of(&s)).await;
            self.mic.release_wake();
            match ended {
                Ended::Heard(rest) => {
                    eprintln!("maprisma-voice: wake phrase heard");
                    *self.cooldown_until.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + self.timing.cooldown);
                    self.show(false, "cooldown");
                    let _ = self.events.send(WakeEvent::Heard { phrase: s.wake_phrase.clone(), rest: rest.clone() });
                    let host = self.host.clone();
                    let launched = tokio::task::spawn_blocking(move || host.launch(&rest)).await;
                    if let Ok(Err(e)) = launched {
                        eprintln!("maprisma-voice: maprisma-dictation: {e}");
                    }
                }
                Ended::RecorderGone => {
                    self.show(false, "recorder_failed");
                    self.wait_or_stop(self.timing.retry).await;
                }
                Ended::Paused => self.show(false, self.why_waiting(&self.host.settings(), &self.mic.state())),
            }
        }
        self.show(false, "off");
        let host = self.host.clone();
        let _ = tokio::task::spawn_blocking(move || host.unload()).await;
    }

    async fn wait_or_stop(&self, time: Duration) {
        let mut stop_rx = self.stop.subscribe();
        if self.stopping() {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(time) => {}
            _ = stop_rx.changed() => {}
        }
    }

    /// Listens until the phrase is heard or it has to stop; the microphone is closed when this
    /// returns.
    async fn listen(&self, mut capture: Capture, recogniser: Arc<dyn Engine>, setup: Setup) -> Ended {
        let mut segmenter = Segmenter::new(self.segments);
        let mut mic_rx = self.mic.subscribe();
        let mut stop_rx = self.stop.subscribe();
        let mut tick = tokio::time::interval(self.timing.tick);
        tick.tick().await;
        let mut job: Option<tokio::task::JoinHandle<(Result<String, VoiceError>, bool)>> = None;
        let mut waiting: Option<Segment> = None;
        let start = |segment: Segment| {
            let (engine, language) = (recogniser.clone(), setup.language.clone());
            // The audio moves into the job and is dropped there, right after the text came.
            tokio::task::spawn_blocking(move || (engine.transcribe(&segment.pcm, &language), segment.truncated))
        };
        let ended = loop {
            if self.mic.state().sessions > 0 || self.stopping() {
                break Ended::Paused;
            }
            tokio::select! {
                chunk = capture.chunks.recv() => {
                    let Some(chunk) = chunk else { break Ended::RecorderGone };
                    if let Some(segment) = segmenter.feed(&chunk) {
                        if job.is_none() { job = Some(start(segment)) } else { waiting = Some(segment) }
                    }
                }
                done = async { job.as_mut().unwrap().await }, if job.is_some() => {
                    job = None;
                    if let Ok((Ok(text), truncated)) = done {
                        let m = match_phrase(&setup.phrase, &text, max_distance(&setup.sensitivity));
                        if m.matched {
                            // A cut-off stretch's rest is incomplete: the overlay listens instead.
                            break Ended::Heard(if truncated { String::new() } else { m.rest });
                        }
                    }
                    if let Some(segment) = waiting.take() { job = Some(start(segment)) }
                }
                _ = mic_rx.changed() => {}
                _ = stop_rx.changed() => {}
                _ = tick.tick() => {
                    let s = self.host.settings();
                    if !s.wake_enabled || !s.enabled || Setup::of(&s) != setup || self.host.speaking() {
                        break Ended::Paused;
                    }
                }
            }
        };
        (capture.stop)();
        drop(capture.chunks);
        if let Some(job) = job {
            job.abort();
        }
        ended
    }

    /// `WakeTest`: listens once (at most `seconds`) for one stretch of speech and says what the
    /// wake recogniser heard and how close it is to the phrase. Claims the microphone like a
    /// session (the listener pauses); nothing is started on a match.
    pub async fn test(&self, seconds: u32, limit: Duration) -> Result<TestResult, VoiceError> {
        let _one = self.testing.try_lock().map_err(|_| VoiceError::new("failed", "busy: a wake test runs"))?;
        let s = self.host.settings();
        let host = self.host.clone();
        let settings = s.clone();
        let recogniser = tokio::task::spawn_blocking(move || host.recogniser(&settings)).await.map_err(|e| VoiceError::engine(e.to_string()))?.map_err(|code| VoiceError::not_ready(crate::i18n::tr(why_not_key(code))))?;
        if self.mic.state().sessions > 0 {
            return Err(VoiceError::new("failed", "busy: a session is listening"));
        }
        let claim = self.mic.claim(limit).await?;
        let host = self.host.clone();
        let microphone = s.microphone.clone();
        let mut capture = tokio::task::spawn_blocking(move || host.capture(&microphone)).await.map_err(|e| VoiceError::recorder(e.to_string()))??;
        let mut segmenter = Segmenter::new(self.segments);
        let until = tokio::time::Instant::now() + Duration::from_secs(u64::from(seconds.clamp(1, 10)));
        let mut mic_rx = self.mic.subscribe();
        let segment = loop {
            if self.mic.state().sessions > 1 {
                (capture.stop)();
                return Err(VoiceError::new("failed", "busy: a session started"));
            }
            tokio::select! {
                _ = mic_rx.changed() => {}
                _ = tokio::time::sleep_until(until) => break None,
                chunk = capture.chunks.recv() => match chunk {
                    None => break None,
                    Some(chunk) => if let Some(segment) = segmenter.feed(&chunk) { break Some(segment) },
                },
            }
        };
        (capture.stop)();
        drop(capture.chunks);
        drop(claim);
        let needed = ((1.0 - max_distance(&s.wake_sensitivity)) * 100.0).round() / 100.0;
        let mut result = TestResult { heard: String::new(), matched: false, score: 0.0, needed, rest: String::new(), phrase: s.wake_phrase.clone(), model: recogniser.model(), speech: segment.is_some() };
        if let Some(segment) = segment {
            let (engine, language) = (recogniser.clone(), s.wake_language.clone());
            let heard = tokio::task::spawn_blocking(move || engine.transcribe(&segment.pcm, &language)).await.map_err(|e| VoiceError::engine(e.to_string()))??;
            let m = match_phrase(&s.wake_phrase, &heard, max_distance(&s.wake_sensitivity));
            result = TestResult { heard, matched: m.matched, score: m.score, rest: m.rest, ..result };
        }
        // The recogniser stays loaded only while the wake phrase is on.
        if !self.host.settings().wake_enabled {
            let host = self.host.clone();
            let _ = tokio::task::spawn_blocking(move || host.unload()).await;
        }
        Ok(result)
    }
}

/// The catalog key for a `wake_why_not` code (docs/protocol.md).
pub fn why_not_key(code: &str) -> &'static str {
    match code {
        "" => "Listening for the wake phrase.",
        "off" => "The wake phrase is off.",
        "disabled" => "Dictation is turned off.",
        "busy" => "Paused while the microphone is in use.",
        "speaking" => "Paused while an answer is read aloud.",
        "cooldown" => "The wake phrase was just heard.",
        "no_whisper_server" => "whisper-server was not found.",
        "no_model" => "No Whisper model was found.",
        "no_recorder" => "No audio recorder (pw-record, parecord or arecord) was found.",
        "recorder_failed" => "The microphone could not be opened.",
        _ => "The wake phrase is not ready.",
    }
}

pub const WHY_NOT_CODES: [&str; 11] = ["", "off", "disabled", "busy", "speaking", "cooldown", "no_whisper_server", "no_model", "no_recorder", "recorder_failed", "failed"];

// ------------------------------------------------------------------------- the real host

/// The wake recogniser's model: the `wake_model` setting when installed, else the smallest
/// installed one (tiny, then base, then small, then anything but large, then the first).
pub fn pick_model<'a>(models: &'a [ModelFile], setting: &str) -> Option<&'a ModelFile> {
    if !setting.is_empty() {
        let file = crate::models::catalog_entry(setting).map_or(setting, |m| m.file);
        if let Some(m) = models.iter().find(|m| m.name == file) {
            return Some(m);
        }
    }
    let first = |prefix: &str| {
        let mut list: Vec<&ModelFile> = models.iter().filter(|m| m.name.starts_with(prefix)).collect();
        // The plain file before its variants (ggml-base.bin before ggml-base.en-q5_1.bin).
        list.sort_by_key(|m| (m.name.len(), m.name.clone()));
        list.first().copied()
    };
    first("ggml-tiny")
        .or_else(|| first("ggml-base"))
        .or_else(|| first("ggml-small"))
        .or_else(|| models.iter().filter(|m| !m.name.contains("large")).min_by_key(|m| m.size))
        .or_else(|| models.iter().min_by_key(|m| m.size))
}

/// Threads of the wake recogniser's CPU-only whisper-server.
pub const THREADS: usize = 4;

pub struct RealHost {
    pub dirs: Dirs,
    pub server: Arc<crate::engines::whisper::WhisperServer>,
    pub speech: Arc<crate::speech::Speech>,
}

impl RealHost {
    pub fn new(dirs: Dirs, speech: Arc<crate::speech::Speech>) -> Self {
        RealHost { dirs, server: Arc::new(crate::engines::whisper::WhisperServer::cpu(THREADS)), speech }
    }
}

/// The overlay program: `~/.local/bin/maprisma-dictation`, else from PATH.
pub fn overlay_program(dirs: &Dirs) -> Option<std::path::PathBuf> {
    let local = dirs.home.join(".local/bin/maprisma-dictation");
    if crate::paths::is_executable(&local) { Some(local) } else { dirs.which("maprisma-dictation") }
}

/// The overlay's arguments for a match.
pub fn overlay_args(rest: &str) -> Vec<String> {
    let mut args = vec!["--wake".to_string()];
    if !rest.trim().is_empty() {
        args.extend(["--ask-rest".to_string(), rest.trim().to_string()]);
    }
    args
}

/// Starts `program args` on its own (a new process group, not ended with the daemon), with
/// nothing on stdin/stdout/stderr; a thread reaps it. Never a shell.
pub fn spawn_detached(program: &std::path::Path, args: &[String]) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let mut child = std::process::Command::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

impl WakeHost for RealHost {
    fn settings(&self) -> Settings {
        crate::settings::load(&self.dirs)
    }

    fn recogniser(&self, s: &Settings) -> Result<Arc<dyn Engine>, &'static str> {
        let binary = crate::engines::find_whisper_server(&self.dirs, &s.whisper_server).ok_or("no_whisper_server")?;
        let models = crate::models::discover(&self.dirs);
        let model = pick_model(&models, &s.wake_model).ok_or("no_model")?;
        crate::recorder::find(&self.dirs).ok_or("no_recorder")?;
        Ok(Arc::new(crate::engines::whisper::WhisperEngine { server: self.server.clone(), binary, model: model.path.clone() }))
    }

    fn capture(&self, microphone: &str) -> Result<Capture, VoiceError> {
        let recorder = crate::recorder::find(&self.dirs).ok_or_else(|| VoiceError::not_ready(crate::i18n::why_not("no_recorder")))?;
        crate::recorder::start(&recorder, microphone).map_err(|e| VoiceError::recorder(format!("{}: {e}", recorder.name())))
    }

    fn speaking(&self) -> bool {
        self.speech.speaking()
    }

    fn launch(&self, rest: &str) -> std::io::Result<()> {
        let program = overlay_program(&self.dirs).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "maprisma-dictation is not installed"))?;
        spawn_detached(&program, &overlay_args(rest))
    }

    fn unload(&self) {
        self.server.stop();
    }
}

#[cfg(test)]
mod tests;
