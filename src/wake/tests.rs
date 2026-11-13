use super::*;
use crate::audio::synth;
use crate::core::{Backend, Core, End, Mode, Prepared, SessionRequest};
use crate::paths::testdir::TestDir;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// ------------------------------------------------------------------------- text

#[test]
fn normalizing() {
    assert_eq!(normalize("Hey, Linux!"), "hey linux");
    assert_eq!(normalize("  HÉY   Línux… "), "hey linux");
    assert_eq!(normalize("Straße, Größe; ÇA VA?"), "strasse grosse ca va");
    assert_eq!(normalize("it's R2-D2"), "its r2 d2");
    assert_eq!(normalize("Zażółć gęślą"), "zazolc gesla");
    assert_eq!(normalize("?!…"), "");
    assert_eq!(levenshtein("", "abc"), 3);
    assert_eq!(levenshtein("kitten", "sitting"), 3);
    assert_eq!(levenshtein("hey linux", "hey linux"), 0);
    assert_eq!(sound("high linux"), "hai linux");
    assert_eq!(sound("hey hei hay"), "hai hai hai");
    assert_eq!(sound("hallo"), "hallo");
}

fn m(phrase: &str, heard: &str, sensitivity: &str) -> Match {
    match_phrase(phrase, heard, max_distance(sensitivity))
}

#[test]
fn matching_english() {
    for heard in ["hey linux", "Hey, Linux!", "HEY LINUX.", "Hey Linux", "Heylinux.", "Hi Linux!", "High Linux.", "Hei Linux", "Oh, hey Linux."] {
        let r = m("Hey Linux", heard, "normal");
        assert!(r.matched, "{heard}: {r:?}");
        assert_eq!(r.rest, "", "{heard}");
    }
    assert_eq!(m("Hey Linux", "Hey, Linux!", "strict").score, 1.0);
    // The request after the phrase is passed on, as heard.
    let r = m("Hey Linux", "Hey Linux, open Firefox.", "normal");
    assert!(r.matched);
    assert_eq!(r.rest, "open Firefox.");
    assert_eq!(m("Hey Linux", "High linux, what's the weather in Berlin?", "normal").rest, "what's the weather in Berlin?");
    // Not the phrase.
    for heard in ["", "Thank you.", "hallo linux", "Linux is great", "I said hey linux", "Hey Siri", "Hello", "Hey", "Linux"] {
        let r = m("Hey Linux", heard, "normal");
        assert!(!r.matched, "{heard}: {r:?}");
        assert_eq!(r.rest, "");
    }
    // Sensitivity: "Hey Lennox" is near but not near enough unless loose.
    assert!(!m("Hey Linux", "Hey Lennox", "strict").matched);
    assert!(!m("Hey Linux", "Hey Lennox", "normal").matched);
    assert!(m("Hey Linux", "Hey Lennox", "loose").matched);
    assert!(m("Hey Linux", "Hey Linox", "strict").matched);
    assert!(m("Hey Linux", "hey linux", "normal").score > m("Hey Linux", "hey linox", "normal").score);
}

#[test]
fn matching_german() {
    let r = m("Hallo Rechner", "Hallo, Rechner! Wie spät ist es?", "normal");
    assert!(r.matched);
    assert_eq!(r.rest, "Wie spät ist es?");
    assert!(m("Hallo Rechner", "Hallo Rechna.", "normal").matched);
    assert!(m("Hallo Rechner", "hallo rechner", "strict").matched);
    assert!(!m("Hallo Rechner", "Hallo Linux", "loose").matched);
    // "Hallo Linux" is not "Hey Linux", and the other way round.
    assert!(!m("Hey Linux", "Hallo Linux", "normal").matched);
    assert!(!m("Hallo Linux", "Hey Linux", "normal").matched);
    assert!(m("Hallo Linux", "Hallo, Linux!", "normal").matched);
    // Umlauts and ß fold either way.
    assert!(m("Grüß dich", "Gruss dich!", "normal").matched);
    let r = m("Hey Linux", "Hey Linux, öffne Firefox.", "normal");
    assert_eq!(r.rest, "öffne Firefox.");
    // A one-word phrase.
    assert!(m("Computer", "Computer, mach das Licht an", "normal").matched);
    assert_eq!(m("Computer", "Computer, mach das Licht an", "normal").rest, "mach das Licht an");
    assert!(!m("Computer", "Komm pute", "normal").matched);
    // The rest is capped.
    let long = format!("Hey Linux {}", "a".repeat(900));
    assert_eq!(m("Hey Linux", &long, "normal").rest.chars().count(), MAX_REST);
}

// ------------------------------------------------------------------------- segments

fn segments(pcm: &[i16]) -> Vec<Segment> {
    let mut s = Segmenter::new(SegmentConfig::default());
    pcm.chunks(crate::audio::CHUNK_SAMPLES).filter_map(|c| s.feed(c)).collect()
}

#[test]
fn cutting_segments() {
    // Speech between quiet: one stretch with 300 ms before it and the 600 ms pause after it.
    let pcm = [synth::noise(1000, 60), synth::tone(1200, 220.0, 6000.0), synth::noise(1500, 60)].concat();
    let s = segments(&pcm);
    assert_eq!(s.len(), 1);
    assert!(!s[0].truncated);
    assert_eq!(crate::audio::ms_of(s[0].pcm.len()), 300 + 1200 + 600);
    // Too short (a click, a cough): nothing.
    assert!(segments(&[synth::silence(1000), synth::tone(250, 220.0, 6000.0), synth::silence(1500)].concat()).is_empty());
    // Two phrases with a pause between them.
    let two = [synth::silence(800), synth::tone(800, 220.0, 6000.0), synth::silence(900), synth::tone(700, 300.0, 6000.0), synth::silence(900)].concat();
    assert_eq!(segments(&two).len(), 2);
    // Short pauses inside a phrase don't cut it.
    let one = [synth::silence(800), synth::tone(500, 220.0, 6000.0), synth::silence(300), synth::tone(500, 220.0, 6000.0), synth::silence(900)].concat();
    assert_eq!(segments(&one).len(), 1);
    // Long speech: its first 3 s once (cut off), then nothing until a pause; then again.
    let long = [synth::silence(800), synth::tone(7000, 220.0, 6000.0), synth::silence(900), synth::tone(800, 220.0, 6000.0), synth::silence(900)].concat();
    let s = segments(&long);
    assert_eq!(s.len(), 2, "{:?}", s.iter().map(|x| (x.pcm.len(), x.truncated)).collect::<Vec<_>>());
    assert!(s[0].truncated && !s[1].truncated);
    assert_eq!(crate::audio::ms_of(s[0].pcm.len()), 300 + 3000);
    // A noisy room is no speech; speech well above it is.
    let room = [synth::noise(3000, 1200), synth::tone(800, 200.0, 12000.0), synth::noise(1500, 1200)].concat();
    assert_eq!(segments(&room).len(), 1);
    assert!(segments(&synth::noise(6000, 1200)).is_empty());
}

#[test]
fn the_kokoro_phrase_is_one_segment() {
    let pcm = crate::audio::from_le_bytes(include_bytes!("../../tests/fixtures/wake/hey-linux.raw"));
    let s = segments(&[synth::silence(500), pcm, synth::silence(1500)].concat());
    assert_eq!(s.len(), 1);
    assert!(!s[0].truncated);
}

// ------------------------------------------------------------------------- model, overlay

fn model(name: &str, size: u64) -> ModelFile {
    ModelFile { name: name.into(), path: format!("/m/{name}").into(), size, managed: true }
}

#[test]
fn smallest_model() {
    let turbo = model("ggml-large-v3-turbo-q5_0.bin", 574);
    let base = model("ggml-base.bin", 148);
    let base_en = model("ggml-base.en-q5_1.bin", 60);
    let small = model("ggml-small.bin", 488);
    let tiny = model("ggml-tiny.bin", 75);
    assert_eq!(pick_model(&[turbo.clone(), base.clone(), small.clone()], "").unwrap().name, "ggml-base.bin");
    assert_eq!(pick_model(&[turbo.clone(), base_en.clone(), base.clone()], "").unwrap().name, "ggml-base.bin");
    assert_eq!(pick_model(&[turbo.clone(), base.clone(), tiny.clone()], "").unwrap().name, "ggml-tiny.bin");
    assert_eq!(pick_model(&[turbo.clone(), small.clone()], "").unwrap().name, "ggml-small.bin");
    assert_eq!(pick_model(std::slice::from_ref(&turbo), "").unwrap().name, turbo.name);
    assert!(pick_model(&[], "").is_none());
    // The setting wins when it is installed (id or file), else the smallest.
    assert_eq!(pick_model(&[turbo.clone(), base.clone()], "turbo").unwrap().name, turbo.name);
    assert_eq!(pick_model(&[turbo.clone(), small.clone(), base.clone()], "ggml-small.bin").unwrap().name, "ggml-small.bin");
    assert_eq!(pick_model(&[turbo, base], "small").unwrap().name, "ggml-base.bin");
}

#[test]
fn overlay_command() {
    assert_eq!(overlay_args(""), ["--wake"]);
    assert_eq!(overlay_args("  "), ["--wake"]);
    assert_eq!(overlay_args("open Firefox; rm -rf ~"), ["--wake", "--ask-rest", "open Firefox; rm -rf ~"]);
    let t = TestDir::new("wake-overlay");
    let bin = t.script("maprisma-dictation", "#!/bin/sh\n");
    let dirs = Dirs::rooted(t.path(), bin.to_str().unwrap());
    assert_eq!(overlay_program(&dirs), Some(bin.join("maprisma-dictation")));
    let local = dirs.home.join(".local/bin");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::copy(bin.join("maprisma-dictation"), local.join("maprisma-dictation")).unwrap();
    assert_eq!(overlay_program(&dirs), Some(local.join("maprisma-dictation")));
    assert_eq!(overlay_program(&Dirs::rooted(&t.path().join("none"), "/nowhere")), None);
    // Detached and reaped: the arguments arrive as they are (no shell).
    let log = t.path().join("argv.log");
    let program = t.script("argv", &format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > {}\n", log.display()));
    spawn_detached(&program.join("argv"), &overlay_args("open Firefox; echo pwned")).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while !log.exists() || std::fs::read_to_string(&log).unwrap().lines().count() < 3 {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "--wake\n--ask-rest\nopen Firefox; echo pwned\n");
}

// ------------------------------------------------------------------------- the listener

struct FakeRecogniser {
    texts: Arc<Mutex<VecDeque<String>>>,
    heard_ms: Arc<Mutex<Vec<u64>>>,
}

impl Engine for FakeRecogniser {
    fn id(&self) -> &'static str {
        "whisper"
    }
    fn model(&self) -> String {
        "ggml-tiny.bin".into()
    }
    fn transcribe(&self, pcm: &[i16], _language: &str) -> Result<String, VoiceError> {
        self.heard_ms.lock().unwrap().push(crate::audio::ms_of(pcm.len()));
        Ok(self.texts.lock().unwrap().pop_front().unwrap_or_default())
    }
}

#[derive(Default)]
struct FakeHost {
    settings: Mutex<Settings>,
    texts: Arc<Mutex<VecDeque<String>>>,
    heard_ms: Arc<Mutex<Vec<u64>>>,
    /// Each capture gets the next (samples, then silence until stopped); then silence only.
    audio: Mutex<VecDeque<Vec<i16>>>,
    opened: AtomicUsize,
    stopped: Arc<AtomicUsize>,
    speaking: AtomicBool,
    launches: Mutex<Vec<String>>,
    unloads: AtomicUsize,
    missing: Mutex<Option<&'static str>>,
}

impl FakeHost {
    fn new() -> Arc<FakeHost> {
        let host = FakeHost::default();
        host.settings.lock().unwrap().wake_enabled = true;
        Arc::new(host)
    }
    fn open_now(&self) -> usize {
        self.opened.load(Ordering::SeqCst) - self.stopped.load(Ordering::SeqCst)
    }
}

impl WakeHost for FakeHost {
    fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }
    fn recogniser(&self, _settings: &Settings) -> Result<Arc<dyn Engine>, &'static str> {
        if let Some(code) = *self.missing.lock().unwrap() {
            return Err(code);
        }
        Ok(Arc::new(FakeRecogniser { texts: self.texts.clone(), heard_ms: self.heard_ms.clone() }))
    }
    fn capture(&self, _microphone: &str) -> Result<Capture, VoiceError> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        let samples = self.audio.lock().unwrap().pop_front().unwrap_or_default();
        let mut capture = Capture::from_samples(samples, true);
        let (inner, stopped) = (std::mem::replace(&mut capture.stop, Box::new(|| {})), self.stopped.clone());
        capture.stop = Box::new(move || {
            inner();
            stopped.fetch_add(1, Ordering::SeqCst);
        });
        Ok(capture)
    }
    fn speaking(&self) -> bool {
        self.speaking.load(Ordering::SeqCst)
    }
    fn launch(&self, rest: &str) -> std::io::Result<()> {
        self.launches.lock().unwrap().push(rest.into());
        Ok(())
    }
    fn unload(&self) {
        self.unloads.fetch_add(1, Ordering::SeqCst);
    }
}

fn quick() -> Timing {
    Timing { cooldown: Duration::from_millis(400), after_session: Duration::from_millis(300), tick: Duration::from_millis(40), retry: Duration::from_millis(200) }
}

fn phrase_audio() -> Vec<i16> {
    [synth::silence(500), synth::tone(1000, 220.0, 6000.0), synth::silence(1000)].concat()
}

fn start(host: Arc<FakeHost>, mic: Arc<MicShare>) -> (Arc<Wake>, mpsc::UnboundedReceiver<WakeEvent>, tokio::task::JoinHandle<()>) {
    let (wake, rx) = Wake::new(host, mic, SegmentConfig::default(), quick());
    let task = tokio::spawn(wake.clone().run());
    (wake, rx, task)
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn next_heard(rx: &mut mpsc::UnboundedReceiver<WakeEvent>) -> (String, String) {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("no Wake").unwrap() {
            WakeEvent::Heard { phrase, rest } => return (phrase, rest),
            WakeEvent::Listening(_) => {}
        }
    }
}

#[tokio::test]
async fn hears_the_phrase_and_starts_the_overlay() {
    let host = FakeHost::new();
    host.audio.lock().unwrap().push_back(phrase_audio());
    host.texts.lock().unwrap().push_back("Hey Linux, open Firefox.".into());
    let mic = Arc::new(MicShare::default());
    let (wake, mut rx, task) = start(host.clone(), mic.clone());
    assert_eq!(rx.recv().await, Some(WakeEvent::Listening(true)));
    assert_eq!(next_heard(&mut rx).await, ("Hey Linux".into(), "open Firefox.".into()));
    until("the launch", || host.launches.lock().unwrap().len() == 1).await;
    assert_eq!(*host.launches.lock().unwrap(), ["open Firefox."]);
    // The microphone closed at once; the cooldown keeps it closed for a moment.
    assert_eq!(host.open_now(), 0);
    assert!(!mic.state().wake_holds);
    let status = wake.status();
    assert_eq!((status["wake_enabled"].as_bool(), status["wake_active"].as_bool(), status["wake_why_not"].as_str()), (Some(true), Some(false), Some("cooldown")));
    assert_eq!(status["wake_phrase"], "Hey Linux");
    // Only the stretch of speech was transcribed (300 ms before it, 1000 ms speech, the 600 ms pause).
    assert_eq!(*host.heard_ms.lock().unwrap(), [1900]);
    // After the cooldown it listens again.
    until("listening again", || host.opened.load(Ordering::SeqCst) == 2 && host.open_now() == 1).await;
    assert_eq!(wake.status()["wake_active"], true);
    wake.shutdown();
    tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
    assert_eq!(host.open_now(), 0);
    assert_eq!(wake.status()["wake_why_not"], "off");
}

#[tokio::test]
async fn other_speech_and_long_speech() {
    let host = FakeHost::new();
    // A sentence that isn't the phrase, then 6 s of talking that starts with it.
    host.audio.lock().unwrap().push_back([phrase_audio(), synth::tone(6000, 220.0, 6000.0), synth::silence(1000)].concat());
    host.texts.lock().unwrap().extend(["Hallo Welt, wie geht es dir?".to_string(), "Hey Linux, what is the weather in".to_string()]);
    let (wake, mut rx, _task) = start(host.clone(), Arc::new(MicShare::default()));
    // The cut-off request is not passed on: the overlay listens instead.
    assert_eq!(next_heard(&mut rx).await, ("Hey Linux".into(), String::new()));
    assert_eq!(host.heard_ms.lock().unwrap().len(), 2);
    assert_eq!(host.heard_ms.lock().unwrap()[1], 300 + 3000);
    until("the launch", || host.launches.lock().unwrap().len() == 1).await;
    assert_eq!(*host.launches.lock().unwrap(), [""]);
    wake.shutdown();
}

#[tokio::test]
async fn off_not_ready_and_settings_changes() {
    let host = FakeHost::new();
    host.settings.lock().unwrap().wake_enabled = false;
    let (wake, _rx, _task) = start(host.clone(), Arc::new(MicShare::default()));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(host.opened.load(Ordering::SeqCst), 0);
    assert_eq!((wake.status()["wake_active"].as_bool(), wake.status()["wake_why_not"].as_str()), (Some(false), Some("off")));
    // Dictation off disables it too.
    {
        let mut s = host.settings.lock().unwrap();
        s.wake_enabled = true;
        s.enabled = false;
    }
    until("disabled", || wake.status()["wake_why_not"] == "disabled").await;
    // No model: says so, doesn't open the microphone.
    *host.missing.lock().unwrap() = Some("no_model");
    host.settings.lock().unwrap().enabled = true;
    until("no_model", || wake.status()["wake_why_not"] == "no_model").await;
    assert_eq!(host.opened.load(Ordering::SeqCst), 0);
    *host.missing.lock().unwrap() = None;
    until("listening", || host.open_now() == 1).await;
    // A new phrase (or microphone, model …) restarts the listener with it.
    host.settings.lock().unwrap().wake_phrase = "Hallo Rechner".into();
    until("restarted", || host.opened.load(Ordering::SeqCst) == 2 && host.open_now() == 1).await;
    assert_eq!(wake.status()["wake_phrase"], "Hallo Rechner");
    // Turned off: the microphone closes and the recogniser is unloaded.
    host.settings.lock().unwrap().wake_enabled = false;
    until("closed", || host.open_now() == 0).await;
    until("unloaded", || host.unloads.load(Ordering::SeqCst) == 1).await;
    assert_eq!(wake.status()["wake_why_not"], "off");
    wake.shutdown();
}

#[tokio::test]
async fn pauses_while_an_answer_is_read_aloud() {
    let host = FakeHost::new();
    let (wake, mut rx, _task) = start(host.clone(), Arc::new(MicShare::default()));
    until("listening", || host.open_now() == 1).await;
    host.speaking.store(true, Ordering::SeqCst);
    until("paused", || host.open_now() == 0).await;
    assert_eq!(wake.status()["wake_why_not"], "speaking");
    host.speaking.store(false, Ordering::SeqCst);
    until("resumed", || host.open_now() == 1).await;
    // The tray hears each change.
    let mut seen = vec![];
    while let Ok(e) = rx.try_recv() {
        seen.push(e);
    }
    assert_eq!(seen, [WakeEvent::Listening(true), WakeEvent::Listening(false), WakeEvent::Listening(true)]);
    wake.shutdown();
}

/// A session backend whose capture checks that the wake listener let go of the microphone first.
struct CheckingBackend {
    engine: Arc<crate::core::tests::FakeEngine>,
    mic: std::sync::OnceLock<Arc<MicShare>>,
    overlaps: AtomicUsize,
    captures: AtomicUsize,
}

impl Backend for CheckingBackend {
    fn prepare(&self, _request: &SessionRequest) -> Result<Prepared, VoiceError> {
        Ok(Prepared { engine: self.engine.clone(), language: "auto".into(), microphone: "default".into() })
    }
    fn capture(&self, _microphone: &str) -> Result<Capture, VoiceError> {
        self.captures.fetch_add(1, Ordering::SeqCst);
        if self.mic.get().unwrap().state().wake_holds {
            self.overlaps.fetch_add(1, Ordering::SeqCst);
        }
        Ok(Capture::from_samples(vec![], true))
    }
    fn status(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }
}

#[tokio::test]
async fn sessions_preempt_the_listener_and_it_resumes_after() {
    let host = FakeHost::new();
    let backend = Arc::new(CheckingBackend { engine: Arc::new(crate::core::tests::FakeEngine::new("x")), mic: Default::default(), overlaps: AtomicUsize::new(0), captures: AtomicUsize::new(0) });
    let (core, mut events) = Core::new(backend.clone(), crate::vad::VadConfig::default());
    backend.mic.set(core.mic.clone()).ok().unwrap();
    let (wake, _rx, _task) = start(host.clone(), core.mic.clone());
    until("listening", || host.open_now() == 1).await;
    // A session from any app: the listener closes the microphone before the session opens it.
    let request = SessionRequest { app_id: "com.maprisma.Launcher".into(), mode: Mode::Dictate, end: End::Release, language: None };
    let id = core.start_session(request.clone()).await.unwrap();
    assert_eq!(host.open_now(), 0);
    assert_eq!(backend.overlaps.load(Ordering::SeqCst), 0);
    assert_eq!(backend.captures.load(Ordering::SeqCst), 1);
    until("busy", || wake.status()["wake_why_not"] == "busy").await;
    // Paused while it runs.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(host.opened.load(Ordering::SeqCst), 1);
    core.cancel(&id).unwrap();
    loop {
        if let crate::core::Event::Closed { .. } = events.recv().await.unwrap() {
            break;
        }
    }
    let closed = Instant::now();
    // Back about 1 s (here 300 ms) after the session closed.
    until("resumed", || host.open_now() == 1).await;
    assert!(closed.elapsed() >= Duration::from_millis(280), "{:?}", closed.elapsed());
    // Again, many times in a row: never two recordings at once.
    for _ in 0..5 {
        until("listening", || host.open_now() == 1).await;
        let id = core.start_session(request.clone()).await.unwrap();
        assert_eq!(host.open_now(), 0);
        core.cancel(&id).unwrap();
    }
    assert_eq!(backend.overlaps.load(Ordering::SeqCst), 0);
    wake.shutdown();
}

#[tokio::test]
async fn cooldown_means_one_wake() {
    let host = FakeHost::new();
    // The phrase twice in a row, then again after the cooldown.
    host.audio.lock().unwrap().push_back([phrase_audio(), phrase_audio()].concat());
    host.audio.lock().unwrap().push_back(phrase_audio());
    host.texts.lock().unwrap().extend(["Hey Linux".to_string(), "Hey Linux".to_string()]);
    let (wake, mut rx, _task) = start(host.clone(), Arc::new(MicShare::default()));
    let first = Instant::now();
    next_heard(&mut rx).await;
    next_heard(&mut rx).await;
    // The second one only after the 400 ms cooldown, from a fresh capture.
    assert!(first.elapsed() >= Duration::from_millis(400));
    // `Heard` is sent just before the overlay is launched: wait for the launch, don't race it.
    until("second launch", || host.launches.lock().unwrap().len() == 2).await;
    assert_eq!(host.opened.load(Ordering::SeqCst), 2);
    wake.shutdown();
}

#[tokio::test]
async fn wake_test_reports_what_was_heard() {
    let host = FakeHost::new();
    host.settings.lock().unwrap().wake_enabled = false;
    host.audio.lock().unwrap().push_back(phrase_audio());
    host.texts.lock().unwrap().push_back("High Linux.".into());
    let mic = Arc::new(MicShare::default());
    let (wake, _rx) = Wake::new(host.clone(), mic.clone(), SegmentConfig::default(), quick());
    let r = wake.test(10, Duration::from_secs(1)).await.unwrap();
    assert_eq!((r.heard.as_str(), r.matched, r.score, r.needed, r.speech), ("High Linux.", true, 1.0, 0.75, true));
    assert_eq!((r.phrase.as_str(), r.model.as_str(), r.rest.as_str()), ("Hey Linux", "ggml-tiny.bin", ""));
    // Nothing was started, and the microphone is free again.
    assert!(host.launches.lock().unwrap().is_empty());
    assert_eq!(host.open_now(), 0);
    assert_eq!(mic.state().sessions, 0);
    // Only quiet: no speech within the time.
    let started = Instant::now();
    let r = wake.test(1, Duration::from_secs(1)).await.unwrap();
    assert!(!r.speech && !r.matched && r.heard.is_empty());
    assert!(started.elapsed() < Duration::from_secs(3));
    // Not ready: the message says why.
    *host.missing.lock().unwrap() = Some("no_whisper_server");
    assert_eq!(wake.test(1, Duration::from_secs(1)).await.unwrap_err().code, "not_ready");
}

#[tokio::test]
async fn wake_test_pauses_the_listener() {
    let host = FakeHost::new();
    host.audio.lock().unwrap().push_back(vec![]);
    host.audio.lock().unwrap().push_back(phrase_audio());
    host.texts.lock().unwrap().push_back("Hey Linux".into());
    let (wake, mut rx, _task) = start(host.clone(), Arc::new(MicShare::default()));
    until("listening", || host.open_now() == 1).await;
    let r = wake.test(5, Duration::from_secs(2)).await.unwrap();
    assert!(r.matched);
    // The test's own match starts nothing; the listener comes back afterwards.
    until("resumed", || host.opened.load(Ordering::SeqCst) == 3 && host.open_now() == 1).await;
    assert!(host.launches.lock().unwrap().is_empty());
    while let Ok(e) = rx.try_recv() {
        assert!(!matches!(e, WakeEvent::Heard { .. }));
    }
    wake.shutdown();
}

#[test]
fn every_why_not_has_a_message() {
    for code in WHY_NOT_CODES {
        let key = why_not_key(code);
        let en = crate::i18n::tr_in(0, key);
        assert_eq!(en, key, "{code} must be a catalog key");
        assert_ne!(crate::i18n::tr_in(1, key), key, "{code} needs German");
    }
}
