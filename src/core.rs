//! Sessions, independent of D-Bus: one listening session at a time (a new one preempts it),
//! recording → level + VAD + partials → transcription → `Final`. Events go out through one
//! channel in order; the D-Bus layer turns them into signals. The [Backend] supplies the engine
//! and the microphone, so tests run with synthetic audio and fake engines.

use crate::VoiceError;
use crate::audio::{self, CHUNK_MS};
use crate::engines::Engine;
use crate::recorder::Capture;
use crate::vad::{Decision, Vad, VadConfig};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// Partial results: at most every 1.2 s of audio, never two at once.
pub const PARTIAL_EVERY_MS: u64 = 1200;
/// Shorter audio isn't sent to the engine (`Final` is empty).
pub const MIN_AUDIO_MS: u64 = 250;
/// External engines: the hard cap (they do their own VAD).
pub const EXTERNAL_MAX: Duration = Duration::from_secs(60);

/// Time limits that don't depend on audio coming in.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// A recorder that delivers no chunk for this long has stalled: listening ends.
    pub stall: Duration,
    /// Slack on top of the VAD's `max_ms`: the wall-clock cap of a recorded session.
    pub wall_slack: Duration,
    /// How long a new session waits for the preempted one to let go of the microphone
    /// (an external engine's stop may take its full 15 s).
    pub release: Duration,
    /// How long shutdown waits for every session to stop.
    pub shutdown: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { stall: Duration::from_secs(5), wall_slack: Duration::from_secs(5), release: Duration::from_secs(20), shutdown: Duration::from_secs(35) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dictate,
    Assistant,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "dictate" => Some(Mode::Dictate),
            "assistant" => Some(Mode::Assistant),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Dictate => "dictate",
            Mode::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The client calls Finish.
    Release,
    /// The VAD finishes after silence.
    Silence,
}

#[derive(Debug, Clone)]
pub struct SessionRequest {
    pub app_id: String,
    pub mode: Mode,
    pub end: End,
    /// Overrides the `language` setting.
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Details {
    pub typed: bool,
    pub engine: String,
    pub model: String,
    pub language: String,
    pub audio_ms: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Partial { id: String, text: String },
    Final { id: String, text: String, details: Details },
    Level { id: String, rms: f64 },
    State { id: String, state: &'static str },
    Error { id: String, code: &'static str, message: String },
    Closed { id: String, reason: String },
}

impl Event {
    pub fn id(&self) -> &str {
        match self {
            Event::Partial { id, .. } | Event::Final { id, .. } | Event::Level { id, .. } | Event::State { id, .. } | Event::Error { id, .. } | Event::Closed { id, .. } => id,
        }
    }
}

pub struct Prepared {
    pub engine: Arc<dyn Engine>,
    pub language: String,
    pub microphone: String,
}

pub trait Backend: Send + Sync + 'static {
    /// Reads the settings and picks the engine (or says why it can't).
    fn prepare(&self, request: &SessionRequest) -> Result<Prepared, VoiceError>;
    /// Opens the microphone.
    fn capture(&self, microphone: &str) -> Result<Capture, VoiceError>;
    /// The engine part of `Status`.
    fn status(&self) -> serde_json::Map<String, serde_json::Value>;
}

enum Control {
    Finish,
    Cancel(&'static str),
    /// Answered `true` once the microphone (or the external recorder) is released.
    Preempt(oneshot::Sender<bool>),
}

struct Live {
    app_id: String,
    mode: Mode,
    state: &'static str,
    engine: String,
    model: String,
    language: String,
    control: mpsc::UnboundedSender<Control>,
}

#[derive(Default)]
struct Inner {
    next: u64,
    sessions: HashMap<String, Live>,
    /// The newest session (the one that may hold the microphone).
    current: Option<String>,
}

pub struct Core {
    backend: Arc<dyn Backend>,
    events: mpsc::UnboundedSender<Event>,
    inner: Arc<Mutex<Inner>>,
    starting: tokio::sync::Mutex<()>,
    vad: VadConfig,
    limits: Limits,
    prefix: String,
}

impl Core {
    pub fn new(backend: Arc<dyn Backend>, vad: VadConfig) -> (Arc<Core>, mpsc::UnboundedReceiver<Event>) {
        Core::with_limits(backend, vad, Limits::default())
    }

    pub fn with_limits(backend: Arc<dyn Backend>, vad: VadConfig, limits: Limits) -> (Arc<Core>, mpsc::UnboundedReceiver<Event>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let core = Core { backend, events: tx, inner: Arc::default(), starting: tokio::sync::Mutex::new(()), vad, limits, prefix: format!("{started:x}") };
        (Arc::new(core), rx)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts a session and returns its id; a listening session is preempted first (it gets its
    /// `Final` and `Closed("preempted")`).
    pub async fn start_session(self: &Arc<Self>, request: SessionRequest) -> Result<String, VoiceError> {
        let _one_at_a_time = self.starting.lock().await;
        let backend = self.backend.clone();
        let req = request.clone();
        let prepared = tokio::task::spawn_blocking(move || backend.prepare(&req)).await.map_err(|e| VoiceError::engine(e.to_string()))??;
        // Preempt the current listener and wait until it let go of the microphone: never two
        // recorders at once, and an old external stop can't hit the new recording.
        let current = {
            let inner = self.lock();
            inner.current.as_ref().and_then(|id| inner.sessions.get(id)).filter(|l| l.state == "listening").map(|l| l.control.clone())
        };
        if let Some(control) = current {
            let (tx, rx) = oneshot::channel();
            if control.send(Control::Preempt(tx)).is_ok() {
                match tokio::time::timeout(self.limits.release, rx).await {
                    // released, or the session ended by itself (its task is gone)
                    Ok(Ok(true)) | Ok(Err(_)) => {}
                    Ok(Ok(false)) => return Err(VoiceError::engine("the previous session's recording could not be stopped")),
                    Err(_) => return Err(VoiceError::engine("the previous session did not release the microphone in time")),
                }
            }
        }
        let engine = prepared.engine.clone();
        let source = if engine.external() {
            let e = engine.clone();
            tokio::task::spawn_blocking(move || e.start()).await.map_err(|e| VoiceError::engine(e.to_string()))??;
            None
        } else {
            let backend = self.backend.clone();
            let mic = prepared.microphone.clone();
            Some(tokio::task::spawn_blocking(move || backend.capture(&mic)).await.map_err(|e| VoiceError::recorder(e.to_string()))??)
        };
        let (control, control_rx) = mpsc::unbounded_channel();
        let id = {
            let mut inner = self.lock();
            inner.next += 1;
            let id = format!("{}-{}", self.prefix, inner.next);
            inner.sessions.insert(
                id.clone(),
                Live { app_id: request.app_id.clone(), mode: request.mode, state: "listening", engine: engine.id().into(), model: engine.model(), language: prepared.language.clone(), control },
            );
            inner.current = Some(id.clone());
            id
        };
        self.emit(Event::State { id: id.clone(), state: "listening" });
        let core = self.clone();
        let session_id = id.clone();
        tokio::spawn(async move {
            match source {
                Some(capture) => core.run_recorded(session_id, prepared, request.end, capture, control_rx).await,
                None => core.run_external(session_id, prepared, control_rx).await,
            }
        });
        Ok(id)
    }

    pub fn finish(&self, id: &str) -> Result<(), VoiceError> {
        self.control(id, Control::Finish)
    }

    pub fn cancel(&self, id: &str) -> Result<(), VoiceError> {
        self.control(id, Control::Cancel("cancelled"))
    }

    /// Whether the session still runs (not closed yet).
    pub fn is_open(&self, id: &str) -> bool {
        self.lock().sessions.contains_key(id)
    }

    /// Stops every session (recorders and external engines) and waits until they are closed,
    /// at most `Limits::shutdown`. Returns whether all of them closed.
    pub async fn shutdown(&self) -> bool {
        let controls: Vec<_> = self.lock().sessions.values().map(|l| l.control.clone()).collect();
        for c in controls {
            let _ = c.send(Control::Cancel("shutdown"));
        }
        let until = Instant::now() + self.limits.shutdown;
        while !self.lock().sessions.is_empty() {
            if Instant::now() >= until {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        true
    }

    /// The client that started it left the bus.
    pub fn client_gone(&self, id: &str) {
        let _ = self.control(id, Control::Cancel("client_gone"));
    }

    fn control(&self, id: &str, c: Control) -> Result<(), VoiceError> {
        let inner = self.lock();
        let live = inner.sessions.get(id).ok_or_else(|| VoiceError::unknown_session(format!("no session {id}")))?;
        let _ = live.control.send(c);
        Ok(())
    }

    /// The whole `Status`: the backend's part plus the current session.
    pub fn status(&self) -> serde_json::Map<String, serde_json::Value> {
        use serde_json::Value;
        let mut map = self.backend.status();
        let inner = self.lock();
        let live = inner.current.as_ref().and_then(|id| inner.sessions.get(id).map(|l| (id, l)));
        map.insert("state".into(), Value::from(live.map_or("idle", |(_, l)| l.state)));
        map.insert("session_id".into(), Value::from(live.map_or(String::new(), |(id, _)| id.clone())));
        map.insert("active_app".into(), Value::from(live.map_or(String::new(), |(_, l)| l.app_id.clone())));
        map.insert("active_mode".into(), Value::from(live.map_or("", |(_, l)| l.mode.as_str())));
        if let Some((_, l)) = live {
            // What this session really uses.
            map.insert("engine".into(), Value::from(l.engine.clone()));
            map.insert("model".into(), Value::from(l.model.clone()));
            map.insert("language".into(), Value::from(l.language.clone()));
        }
        map.insert("sessions".into(), Value::from(inner.sessions.len() as u64));
        map
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn set_state(&self, id: &str, state: &'static str) {
        if let Some(l) = self.lock().sessions.get_mut(id) {
            l.state = state;
        }
        self.emit(Event::State { id: id.into(), state });
    }

    fn close(&self, id: &str, reason: &str) {
        {
            let mut inner = self.lock();
            inner.sessions.remove(id);
            if inner.current.as_deref() == Some(id) {
                inner.current = None;
            }
        }
        self.emit(Event::Closed { id: id.into(), reason: reason.into() });
    }

    fn fail(&self, id: &str, error: VoiceError) {
        let code = if error.code == "io" { "engine_failed" } else { error.code };
        self.emit(Event::Error { id: id.into(), code, message: error.message });
        self.set_state(id, "error");
        self.close(id, "error");
    }

    async fn run_recorded(self: Arc<Self>, id: String, prepared: Prepared, end: End, mut capture: Capture, mut control: mpsc::UnboundedReceiver<Control>) {
        let started = Instant::now();
        let engine = prepared.engine.clone();
        let mut vad = Vad::new(self.vad, end == End::Silence);
        let mut pcm: Vec<i16> = Vec::new();
        let (partial_tx, mut partial_rx) = mpsc::unbounded_channel::<String>();
        let mut partial_running = false;
        let mut last_partial_ms = 0u64;
        let mut last_partial = String::new();
        let mut preempted: Option<oneshot::Sender<bool>> = None;
        let mut cancel: Option<&'static str> = None;
        let mut recorder_ended = false;
        // Wall-clock limits: the VAD only counts audio that came in, a stalled recorder sends none.
        let hard_end = tokio::time::Instant::now() + Duration::from_millis(u64::from(self.vad.max_ms)) + self.limits.wall_slack;
        let stall = tokio::time::sleep(self.limits.stall);
        tokio::pin!(stall);
        let reason: &'static str = loop {
            tokio::select! {
                _ = tokio::time::sleep_until(hard_end) => break "max_length",
                _ = &mut stall => { recorder_ended = true; break "recorder_stalled" }
                c = control.recv() => match c {
                    Some(Control::Finish) | None => break "finished",
                    Some(Control::Cancel(r)) => { cancel = Some(r); break r }
                    Some(Control::Preempt(ack)) => { preempted = Some(ack); break "preempted" }
                },
                chunk = capture.chunks.recv() => {
                    let Some(chunk) = chunk else { recorder_ended = true; break "recorder_ended" };
                    stall.as_mut().reset(tokio::time::Instant::now() + self.limits.stall);
                    let level = audio::rms(&chunk);
                    let ms = audio::ms_of(chunk.len()) as u32;
                    pcm.extend_from_slice(&chunk);
                    self.emit(Event::Level { id: id.clone(), rms: level });
                    match vad.feed(level, ms.max(1).min(CHUNK_MS * 4)) {
                        Decision::Continue => {}
                        Decision::Finish => break "silence",
                        Decision::MaxLength => break "max_length",
                        Decision::NoSpeech => { cancel = Some("no_speech"); break "no_speech" }
                    }
                    let audio_ms = audio::ms_of(pcm.len());
                    if engine.partials() && !partial_running && vad.heard_speech() && audio_ms >= last_partial_ms + PARTIAL_EVERY_MS {
                        partial_running = true;
                        last_partial_ms = audio_ms;
                        let (e, snapshot, lang, tx) = (engine.clone(), pcm.clone(), prepared.language.clone(), partial_tx.clone());
                        tokio::task::spawn_blocking(move || {
                            let _ = tx.send(e.transcribe(&snapshot, &lang).unwrap_or_default());
                        });
                    }
                },
                Some(text) = partial_rx.recv() => {
                    partial_running = false;
                    if !text.is_empty() && text != last_partial {
                        last_partial = text.clone();
                        self.emit(Event::Partial { id: id.clone(), text });
                    }
                },
            }
        };
        // The microphone goes off now, before transcribing.
        (capture.stop)();
        drop(capture.chunks);
        if let Some(ack) = preempted {
            let _ = ack.send(true);
        }
        if let Some(r) = cancel {
            self.set_state(&id, "cancelled");
            self.close(&id, r);
            return;
        }
        let audio_ms = audio::ms_of(pcm.len());
        if recorder_ended && pcm.is_empty() {
            self.fail(&id, VoiceError::recorder("the recorder stopped before it delivered audio"));
            return;
        }
        self.set_state(&id, "transcribing");
        let result = if audio_ms < MIN_AUDIO_MS {
            Ok(String::new())
        } else {
            let (e, lang) = (engine.clone(), prepared.language.clone());
            let mut job = tokio::task::spawn_blocking(move || e.transcribe(&pcm, &lang));
            loop {
                tokio::select! {
                    r = &mut job => break r.unwrap_or_else(|e| Err(VoiceError::engine(e.to_string()))),
                    c = control.recv() => match c {
                        Some(Control::Cancel(r)) => {
                            self.set_state(&id, "cancelled");
                            self.close(&id, r);
                            return;
                        }
                        // Already off the microphone.
                        Some(Control::Preempt(ack)) => { let _ = ack.send(true); }
                        Some(Control::Finish) => {}
                        None => {}
                    },
                }
            }
        };
        match result {
            Ok(text) => {
                let details = Details { typed: false, engine: engine.id().into(), model: engine.model(), language: prepared.language.clone(), audio_ms, elapsed_ms: started.elapsed().as_millis() as u64 };
                self.emit(Event::Final { id: id.clone(), text, details });
                self.set_state(&id, "done");
                self.close(&id, reason);
            }
            Err(e) => self.fail(&id, e),
        }
    }

    async fn run_external(self: Arc<Self>, id: String, prepared: Prepared, mut control: mpsc::UnboundedReceiver<Control>) {
        let started = Instant::now();
        let engine = prepared.engine.clone();
        let mut preempted = None;
        let (reason, cancel) = tokio::select! {
            c = control.recv() => match c {
                Some(Control::Finish) | None => ("finished", false),
                Some(Control::Cancel(r)) => (r, true),
                Some(Control::Preempt(ack)) => { preempted = Some(ack); ("preempted", false) }
            },
            _ = tokio::time::sleep(EXTERNAL_MAX) => ("max_length", false),
        };
        let e = engine.clone();
        let mut stopped = tokio::task::spawn_blocking(move || e.stop(cancel)).await.unwrap_or_else(|e| Err(VoiceError::engine(e.to_string())));
        if cancel && stopped.is_err() {
            // Cancelling failed (the recorder may still run): stop it the plain way (it may
            // type what it heard), and say so instead of reporting a clean cancel.
            let e = engine.clone();
            let plain = tokio::task::spawn_blocking(move || e.stop(false)).await.unwrap_or_else(|e| Err(VoiceError::engine(e.to_string())));
            let first = stopped.unwrap_err();
            stopped = Err(match plain {
                Ok(()) => VoiceError::engine(format!("cancelling did not work, so it was stopped instead (it may type what it heard): {}", first.message)),
                Err(again) => VoiceError::engine(format!("the recording could not be stopped: {}; {}", first.message, again.message)),
            });
        }
        if let Some(ack) = preempted {
            let _ = ack.send(stopped.is_ok());
        }
        if cancel && stopped.is_ok() {
            self.set_state(&id, "cancelled");
            self.close(&id, reason);
            return;
        }
        match stopped {
            Ok(()) => {
                let details = Details { typed: true, engine: engine.id().into(), model: String::new(), language: prepared.language.clone(), audio_ms: 0, elapsed_ms: started.elapsed().as_millis() as u64 };
                self.emit(Event::Final { id: id.clone(), text: String::new(), details });
                self.set_state(&id, "done");
                self.close(&id, reason);
            }
            Err(e) => self.fail(&id, e),
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::audio::synth;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An engine that returns `text` (or fails) and counts its calls.
    pub struct FakeEngine {
        pub text: String,
        pub fail: bool,
        pub partials: bool,
        pub external: bool,
        pub calls: AtomicUsize,
        pub log: Mutex<Vec<String>>,
        /// External stop takes this long.
        pub stop_delay: Duration,
        /// `stop(true)` fails / `stop(false)` fails.
        pub cancel_fails: bool,
        pub stop_fails: bool,
    }

    impl FakeEngine {
        pub fn new(text: &str) -> Self {
            FakeEngine { text: text.into(), fail: false, partials: false, external: false, calls: AtomicUsize::new(0), log: Mutex::default(), stop_delay: Duration::ZERO, cancel_fails: false, stop_fails: false }
        }
        pub fn external() -> Self {
            FakeEngine { external: true, ..FakeEngine::new("") }
        }
    }

    impl Engine for FakeEngine {
        fn id(&self) -> &'static str {
            if self.external { "voxtype" } else { "whisper" }
        }
        fn model(&self) -> String {
            "fake.bin".into()
        }
        fn external(&self) -> bool {
            self.external
        }
        fn partials(&self) -> bool {
            self.partials
        }
        fn transcribe(&self, pcm: &[i16], language: &str) -> Result<String, VoiceError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.log.lock().unwrap().push(format!("transcribe {} {language}", audio::ms_of(pcm.len())));
            if self.fail { Err(VoiceError::engine("boom")) } else { Ok(format!("{} {}ms", self.text, audio::ms_of(pcm.len()) / 100 * 100)) }
        }
        fn start(&self) -> Result<(), VoiceError> {
            self.log.lock().unwrap().push("start".into());
            Ok(())
        }
        fn stop(&self, cancel: bool) -> Result<(), VoiceError> {
            std::thread::sleep(self.stop_delay);
            self.log.lock().unwrap().push(format!("stop {cancel}"));
            if (cancel && self.cancel_fails) || (!cancel && self.stop_fails) { Err(VoiceError::engine("no daemon")) } else { Ok(()) }
        }
    }

    /// Each session gets the next of `audio` (samples, then endless silence or the end).
    pub struct FakeBackend {
        pub engine: Arc<FakeEngine>,
        pub audio: Mutex<Vec<(Vec<i16>, bool)>>,
        pub captures: AtomicUsize,
        /// The next capture opens and never delivers a chunk (a stalled device).
        pub stall: std::sync::atomic::AtomicBool,
        /// The next capture sends one sample every 20 ms (the VAD's audio clock crawls).
        pub trickle: std::sync::atomic::AtomicBool,
        pub held: Mutex<Vec<mpsc::Sender<Vec<i16>>>>,
    }

    impl Backend for FakeBackend {
        fn prepare(&self, request: &SessionRequest) -> Result<Prepared, VoiceError> {
            Ok(Prepared { engine: self.engine.clone(), language: request.language.clone().unwrap_or("auto".into()), microphone: "default".into() })
        }
        fn capture(&self, _microphone: &str) -> Result<Capture, VoiceError> {
            self.captures.fetch_add(1, Ordering::SeqCst);
            if self.stall.swap(false, Ordering::SeqCst) {
                let (tx, rx) = mpsc::channel(4);
                self.held.lock().unwrap().push(tx);
                return Ok(Capture { chunks: rx, stop: Box::new(|| {}) });
            }
            if self.trickle.swap(false, Ordering::SeqCst) {
                let (tx, rx) = mpsc::channel(4);
                std::thread::spawn(move || while tx.blocking_send(vec![0]).is_ok() {
                    std::thread::sleep(Duration::from_millis(20));
                });
                return Ok(Capture { chunks: rx, stop: Box::new(|| {}) });
            }
            let mut audio = self.audio.lock().unwrap();
            let (samples, more) = if audio.is_empty() { (vec![], true) } else { audio.remove(0) };
            Ok(Capture::from_samples(samples, more))
        }
        fn status(&self) -> serde_json::Map<String, serde_json::Value> {
            let mut m = serde_json::Map::new();
            m.insert("ready".into(), true.into());
            m
        }
    }

    fn setup(engine: FakeEngine, audio: Vec<(Vec<i16>, bool)>) -> (Arc<Core>, mpsc::UnboundedReceiver<Event>, Arc<FakeEngine>) {
        let engine = Arc::new(engine);
        let backend = Arc::new(FakeBackend { engine: engine.clone(), audio: Mutex::new(audio), captures: AtomicUsize::new(0), stall: Default::default(), trickle: Default::default(), held: Mutex::default() });
        let (core, rx) = Core::new(backend, VadConfig::default());
        (core, rx, engine)
    }

    fn fake_backend(engine: Arc<FakeEngine>) -> Arc<FakeBackend> {
        Arc::new(FakeBackend { engine, audio: Mutex::default(), captures: AtomicUsize::new(0), stall: Default::default(), trickle: Default::default(), held: Mutex::default() })
    }

    fn quick() -> Limits {
        Limits { stall: Duration::from_millis(300), wall_slack: Duration::from_millis(200), release: Duration::from_millis(400), shutdown: Duration::from_secs(5) }
    }

    fn request(app: &str, end: End) -> SessionRequest {
        SessionRequest { app_id: app.into(), mode: Mode::Dictate, end, language: None }
    }

    /// Events (without Level) until session `id` is closed.
    async fn until_closed(rx: &mut mpsc::UnboundedReceiver<Event>, id: &str, seen: &mut Vec<Event>) {
        loop {
            let e = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.expect("timed out").expect("channel closed");
            let done = matches!(&e, Event::Closed { id: i, .. } if i == id);
            if !matches!(e, Event::Level { .. }) {
                seen.push(e);
            }
            if done {
                return;
            }
        }
    }

    fn speech_then_silence() -> Vec<i16> {
        [synth::silence(300), synth::tone(1500, 220.0, 6000.0), synth::silence(2000)].concat()
    }

    #[tokio::test]
    async fn silence_mode_finishes_by_itself() {
        let (core, mut rx, engine) = setup(FakeEngine::new("hello"), vec![(speech_then_silence(), true)]);
        let mut req = request("app", End::Silence);
        req.language = Some("de".into());
        let id = core.start_session(req).await.unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        // 300 + 1500 + 900 ms, cut at the VAD's decision.
        let fin = seen.iter().find_map(|e| match e { Event::Final { text, details, .. } => Some((text.clone(), details.clone())), _ => None }).unwrap();
        assert_eq!(fin.0, "hello 2700ms");
        assert_eq!((fin.1.engine.as_str(), fin.1.language.as_str(), fin.1.typed, fin.1.audio_ms), ("whisper", "de", false, 2700));
        let states: Vec<&str> = seen.iter().filter_map(|e| match e { Event::State { state, .. } => Some(*state), _ => None }).collect();
        assert_eq!(states, ["listening", "transcribing", "done"]);
        assert_eq!(seen.last(), Some(&Event::Closed { id: id.clone(), reason: "silence".into() }));
        assert_eq!(engine.calls.load(Ordering::SeqCst), 1);
        assert_eq!(core.status()["state"], "idle");
    }

    #[tokio::test]
    async fn levels_come_per_chunk() {
        let (core, mut rx, _) = setup(FakeEngine::new("x"), vec![(speech_then_silence(), true)]);
        let id = core.start_session(request("app", End::Silence)).await.unwrap();
        let mut levels = vec![];
        loop {
            match rx.recv().await.unwrap() {
                Event::Level { rms, .. } => levels.push(rms),
                Event::Closed { id: i, .. } if i == id => break,
                _ => {}
            }
        }
        assert_eq!(levels.len(), 54); // 2700 ms / 50 ms
        assert!(levels[0] < 0.001 && levels[10] > 0.1);
    }

    #[tokio::test]
    async fn no_speech_cancels() {
        let (core, mut rx, engine) = setup(FakeEngine::new("x"), vec![(synth::noise(5000, 50), true)]);
        let id = core.start_session(request("app", End::Silence)).await.unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        assert!(seen.contains(&Event::State { id: id.clone(), state: "cancelled" }));
        assert_eq!(seen.last(), Some(&Event::Closed { id, reason: "no_speech".into() }));
        assert!(!seen.iter().any(|e| matches!(e, Event::Final { .. })));
        assert_eq!(engine.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn release_mode_waits_for_finish_and_cancel_works() {
        let (core, mut rx, _) = setup(FakeEngine::new("held"), vec![(synth::tone(1000, 220.0, 6000.0), true), (vec![], true)]);
        let id = core.start_session(request("app", End::Release)).await.unwrap();
        // Lots of silence follows; release mode keeps listening.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(core.status()["state"], "listening");
        assert_eq!(core.status()["active_app"], "app");
        core.finish(&id).unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        assert!(seen.iter().any(|e| matches!(e, Event::Final { text, .. } if text.starts_with("held"))));
        assert_eq!(seen.last(), Some(&Event::Closed { id: id.clone(), reason: "finished".into() }));
        assert_eq!(core.finish(&id).unwrap_err().code, "unknown_session");
        // Cancel: no Final.
        let id2 = core.start_session(request("app", End::Release)).await.unwrap();
        core.cancel(&id2).unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &id2, &mut seen).await;
        assert!(!seen.iter().any(|e| matches!(e, Event::Final { .. })));
        assert_eq!(seen.last(), Some(&Event::Closed { id: id2, reason: "cancelled".into() }));
    }

    #[tokio::test]
    async fn preemption_gives_the_old_session_its_text() {
        let (core, mut rx, _) = setup(FakeEngine::new("old"), vec![(synth::tone(1000, 220.0, 6000.0), true), (speech_then_silence(), true)]);
        let first = core.start_session(request("launcher", End::Release)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let second = core.start_session(request("theme", End::Silence)).await.unwrap();
        assert_ne!(first, second);
        let mut seen = vec![];
        until_closed(&mut rx, &first, &mut seen).await;
        assert!(seen.iter().any(|e| matches!(e, Event::Final { id, text, .. } if *id == first && text.starts_with("old"))));
        assert!(seen.contains(&Event::Closed { id: first.clone(), reason: "preempted".into() }));
        assert_eq!(core.finish(&first).unwrap_err().code, "unknown_session");
        // The new one owns the microphone now and runs to its end.
        let mut seen = vec![];
        until_closed(&mut rx, &second, &mut seen).await;
        assert!(seen.contains(&Event::Closed { id: second.clone(), reason: "silence".into() }));
    }

    #[tokio::test]
    async fn partials_while_listening() {
        let mut engine = FakeEngine::new("part");
        engine.partials = true;
        let long = [synth::tone(4000, 220.0, 6000.0), synth::silence(1500)].concat();
        let (core, mut rx, engine) = setup(engine, vec![(long, true)]);
        let id = core.start_session(request("app", End::Silence)).await.unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        let partials: Vec<&String> = seen.iter().filter_map(|e| match e { Event::Partial { text, .. } => Some(text), _ => None }).collect();
        assert!(!partials.is_empty(), "{seen:?}");
        // Partials come before the Final.
        let last_partial = seen.iter().rposition(|e| matches!(e, Event::Partial { .. })).unwrap();
        let fin = seen.iter().position(|e| matches!(e, Event::Final { .. })).unwrap();
        assert!(last_partial < fin);
        assert!(engine.calls.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn engine_error_and_recorder_failure() {
        let mut failing = FakeEngine::new("x");
        failing.fail = true;
        let (core, mut rx, _) = setup(failing, vec![(speech_then_silence(), true), (vec![], false)]);
        let id = core.start_session(request("app", End::Silence)).await.unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        assert!(seen.contains(&Event::Error { id: id.clone(), code: "engine_failed", message: "boom".into() }));
        assert_eq!(seen.last(), Some(&Event::Closed { id, reason: "error".into() }));
        // A recorder that ends without audio.
        let id = core.start_session(request("app", End::Release)).await.unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        assert!(seen.iter().any(|e| matches!(e, Event::Error { code: "recorder_failed", .. })));
    }

    #[tokio::test]
    async fn short_audio_is_not_transcribed() {
        let (core, mut rx, engine) = setup(FakeEngine::new("x"), vec![(synth::tone(100, 220.0, 6000.0), false)]);
        let id = core.start_session(request("app", End::Release)).await.unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        assert!(seen.iter().any(|e| matches!(e, Event::Final { text, .. } if text.is_empty())));
        assert_eq!(seen.last(), Some(&Event::Closed { id, reason: "recorder_ended".into() }));
        assert_eq!(engine.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn external_engines_type_themselves() {
        let engine = Arc::new(FakeEngine::external());
        let backend = fake_backend(engine.clone());
        let (core, mut rx) = Core::new(backend.clone(), VadConfig::default());
        let id = core.start_session(request("app", End::Silence)).await.unwrap();
        core.finish(&id).unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        let details = seen.iter().find_map(|e| match e { Event::Final { text, details, .. } if text.is_empty() => Some(details.clone()), _ => None }).unwrap();
        assert!(details.typed);
        assert_eq!(details.engine, "voxtype");
        // Preempted by another session: stopped (types), Final typed, then the new one runs.
        let a = core.start_session(request("a", End::Release)).await.unwrap();
        let b = core.start_session(request("b", End::Release)).await.unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &a, &mut seen).await;
        assert!(seen.contains(&Event::Closed { id: a, reason: "preempted".into() }));
        core.cancel(&b).unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &b, &mut seen).await;
        assert_eq!(seen.last(), Some(&Event::Closed { id: b, reason: "cancelled".into() }));
        assert_eq!(*engine.log.lock().unwrap(), ["start", "stop false", "start", "stop false", "start", "stop true"]);
        // The microphone was never opened.
        assert_eq!(backend.captures.load(Ordering::SeqCst), 0);
    }

    /// Shutdown stops an external recording (and a recorded one) and waits until they closed.
    #[tokio::test]
    async fn shutdown_stops_every_session() {
        let engine = Arc::new(FakeEngine::external());
        let (core, mut rx) = Core::new(fake_backend(engine.clone()), VadConfig::default());
        let id = core.start_session(request("app", End::Release)).await.unwrap();
        assert!(core.shutdown().await);
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        assert_eq!(seen.last(), Some(&Event::Closed { id, reason: "shutdown".into() }));
        assert_eq!(*engine.log.lock().unwrap(), ["start", "stop true"]);
        assert_eq!(core.status()["sessions"], 0);
        // a recorded one
        let (core, mut rx, _) = setup(FakeEngine::new("x"), vec![(vec![], true)]);
        let id = core.start_session(request("app", End::Release)).await.unwrap();
        assert!(core.shutdown().await);
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        assert!(!seen.iter().any(|e| matches!(e, Event::Final { .. })));
        assert_eq!(core.status()["state"], "idle");
    }

    /// A failed external cancel is not reported as a clean cancel: the plain stop is tried and
    /// the session ends with an error.
    #[tokio::test]
    async fn failed_external_cancel_is_an_error() {
        let mut ext = FakeEngine::external();
        ext.cancel_fails = true;
        let engine = Arc::new(ext);
        let (core, mut rx) = Core::new(fake_backend(engine.clone()), VadConfig::default());
        let id = core.start_session(request("app", End::Release)).await.unwrap();
        core.cancel(&id).unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        assert!(seen.iter().any(|e| matches!(e, Event::Error { message, .. } if message.contains("stopped instead"))), "{seen:?}");
        assert_eq!(seen.last(), Some(&Event::Closed { id, reason: "error".into() }));
        assert_eq!(*engine.log.lock().unwrap(), ["start", "stop true", "stop false"]);
        // both fail: the error says it could not be stopped
        let mut ext = FakeEngine::external();
        ext.cancel_fails = true;
        ext.stop_fails = true;
        let (core, mut rx) = Core::new(fake_backend(Arc::new(ext)), VadConfig::default());
        let id = core.start_session(request("app", End::Release)).await.unwrap();
        core.cancel(&id).unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        assert!(seen.iter().any(|e| matches!(e, Event::Error { message, .. } if message.contains("could not be stopped"))), "{seen:?}");
    }

    /// A recorder that opens but never delivers audio: the stall limit ends listening (no
    /// endless session), and with no audio it is a recorder error.
    #[tokio::test]
    async fn stalled_recorder_ends() {
        let engine = Arc::new(FakeEngine::new("x"));
        let backend = fake_backend(engine);
        backend.stall.store(true, Ordering::SeqCst);
        let (core, mut rx) = Core::with_limits(backend.clone(), VadConfig::default(), quick());
        let started = Instant::now();
        let id = core.start_session(request("app", End::Release)).await.unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(seen.iter().any(|e| matches!(e, Event::Error { code: "recorder_failed", .. })), "{seen:?}");
        assert_eq!(backend.held.lock().unwrap().len(), 1, "the stalled capture's sender is still held");
    }

    /// The wall-clock cap holds even when the VAD's audio clock runs slow.
    #[tokio::test]
    async fn wall_clock_cap() {
        let vad = VadConfig { max_ms: 300, ..VadConfig::default() };
        let engine = Arc::new(FakeEngine::new("x"));
        let backend = fake_backend(engine);
        backend.trickle.store(true, Ordering::SeqCst);
        // 1 ms of audio per 20 ms: the VAD would take 6 s to reach its 300 ms
        let started = Instant::now();
        let (core, mut rx) = Core::with_limits(backend, vad, Limits { stall: Duration::from_secs(5), wall_slack: Duration::from_millis(100), ..quick() });
        let id = core.start_session(request("app", End::Release)).await.unwrap();
        let mut seen = vec![];
        until_closed(&mut rx, &id, &mut seen).await;
        assert_eq!(seen.last(), Some(&Event::Closed { id, reason: "max_length".into() }));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// A new session never starts while the preempted one's recording is still being stopped:
    /// a stop slower than the release limit, or a failed stop, refuses the new session.
    #[tokio::test]
    async fn preemption_waits_for_release() {
        let mut ext = FakeEngine::external();
        ext.stop_delay = Duration::from_millis(1000);
        let engine = Arc::new(ext);
        let (core, mut rx) = Core::with_limits(fake_backend(engine.clone()), VadConfig::default(), quick());
        let a = core.start_session(request("a", End::Release)).await.unwrap();
        let err = core.start_session(request("b", End::Release)).await.unwrap_err();
        assert!(err.message.contains("in time"), "{}", err.message);
        let mut seen = vec![];
        until_closed(&mut rx, &a, &mut seen).await;
        // only one start: the second never ran
        assert_eq!(*engine.log.lock().unwrap(), ["start", "stop false"]);
        // a failed stop refuses too
        let mut ext = FakeEngine::external();
        ext.stop_fails = true;
        let engine = Arc::new(ext);
        let (core, _rx) = Core::with_limits(fake_backend(engine.clone()), VadConfig::default(), quick());
        core.start_session(request("a", End::Release)).await.unwrap();
        let err = core.start_session(request("b", End::Release)).await.unwrap_err();
        assert!(err.message.contains("could not be stopped"), "{}", err.message);
        assert_eq!(*engine.log.lock().unwrap(), ["start", "stop false"]);
        // a quick stop: the new one starts after the old one released
        let engine = Arc::new(FakeEngine::external());
        let (core, _rx) = Core::with_limits(fake_backend(engine.clone()), VadConfig::default(), quick());
        core.start_session(request("a", End::Release)).await.unwrap();
        core.start_session(request("b", End::Release)).await.unwrap();
        assert_eq!(*engine.log.lock().unwrap(), ["start", "stop false", "start"]);
    }
}
