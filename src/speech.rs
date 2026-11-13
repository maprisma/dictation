//! Spoken answers (`Speak`): an installed Kokoro (the Maprisma Narrator's runtime in
//! `~/.local/share/kindle-reader`, used read-only: its resident `kokoro_tts.py --worker`, no
//! bytecode written, output only into our private runtime folder) reads text aloud sentence by
//! sentence through `pw-play` (else `paplay`, `aplay`). One speech at a time: a new `Speak`, a
//! `StopSpeaking` or a new dictation session stops the current one at once.

use crate::paths::Dirs;
use crate::procs::ChildGuard;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The longest text one `Speak` takes.
pub const MAX_TEXT: usize = 8000;

/// How long one sentence may take Kokoro (the first one loads the model). A worker that takes
/// longer is killed; the speech ends with an error.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq)]
pub enum SpeechEvent {
    /// `speaking`, `done`, `stopped` or `error` (with a message).
    State { id: String, state: &'static str, message: String },
}

/// Where Kokoro is, or why it isn't usable.
#[derive(Debug, Clone, PartialEq)]
pub struct Kokoro {
    pub python: PathBuf,
    pub script: PathBuf,
}

/// `runtime` holds `venv/bin/python`, `app/kokoro_tts.py` and `kokoro/*.onnx`.
pub fn find_kokoro(runtime: &Path) -> Result<Kokoro, &'static str> {
    let python = runtime.join("venv/bin/python");
    let script = runtime.join("app/kokoro_tts.py");
    if !crate::paths::is_executable(&python) || !script.is_file() {
        return Err("no_kokoro");
    }
    let has_model = std::fs::read_dir(runtime.join("kokoro"))
        .map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().ends_with(".onnx")))
        .unwrap_or(false);
    if !has_model {
        return Err("no_kokoro_model");
    }
    Ok(Kokoro { python, script })
}

pub fn default_runtime(dirs: &Dirs) -> PathBuf {
    dirs.data_home.join("kindle-reader")
}

/// Kokoro's language code for a Whisper-style one.
pub fn kokoro_language(code: &str) -> String {
    match code {
        "" | "auto" | "de" => "de".into(),
        "en" => "en-us".into(),
        other => other.into(),
    }
}

/// Sentences of at most ~400 characters (Kokoro reads short pieces best; the first one starts
/// playing sooner). Markdown marks and code fences are dropped.
pub fn sentences(text: &str) -> Vec<String> {
    let mut clean = String::new();
    let mut in_code = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let t = t.trim_start_matches(['#', '>', '-', '*', '•']).trim();
        let t: String = t.chars().filter(|c| !matches!(c, '*' | '`' | '_')).collect();
        if !t.is_empty() {
            if !clean.is_empty() {
                clean.push(' ');
            }
            clean.push_str(&t);
            if !t.ends_with(['.', '!', '?', ':', ';']) {
                clean.push('.');
            }
        }
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = clean.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        current.push(*c);
        let end = matches!(c, '.' | '!' | '?') && chars.get(i + 1).is_none_or(|n| n.is_whitespace());
        if (end && current.trim().chars().count() >= 2) || current.chars().count() >= 400 {
            out.push(current.trim().to_string());
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

/// The worker's pipes. While a sentence is being made they are out of `Inner` (the lock is
/// never held during worker I/O), so Stop, Status and a new session never wait for Kokoro.
struct WorkerIo {
    stdin: ChildStdin,
    stdout: ChildStdout,
    pending: Vec<u8>,
}

impl WorkerIo {
    /// One reply line, or an error when the worker ends or `deadline` passes.
    fn read_line(&mut self, deadline: Instant) -> Result<String, &'static str> {
        loop {
            if let Some(n) = self.pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=n).collect();
                return Ok(String::from_utf8_lossy(&line).into_owned());
            }
            if self.pending.len() > 64 * 1024 {
                return Err("Kokoro: reply too long");
            }
            let now = Instant::now();
            if now >= deadline {
                return Err("Kokoro did not answer in time");
            }
            let wait = (deadline - now).min(Duration::from_millis(50));
            let mut fd = libc::pollfd { fd: self.stdout.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd for an fd we own.
            let ready = unsafe { libc::poll(&mut fd, 1, wait.as_millis() as libc::c_int) };
            if ready <= 0 {
                continue;
            }
            let mut buf = [0u8; 4096];
            match self.stdout.read(&mut buf) {
                Ok(0) => return Err("Kokoro stopped"),
                Ok(n) => self.pending.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err("Kokoro stopped"),
            }
        }
    }
}

#[derive(Default)]
struct Inner {
    worker: Option<ChildGuard>,
    /// None while a sentence is in flight (or no worker).
    io: Option<WorkerIo>,
    player: Option<ChildGuard>,
    current: Option<String>,
    used: Option<Instant>,
}

pub struct Speech {
    dirs: Dirs,
    inner: Mutex<Inner>,
    /// Bumped by every Speak/Stop: a running speech ends when it no longer matches.
    generation: AtomicU64,
    counter: AtomicU64,
    events: tokio::sync::mpsc::UnboundedSender<SpeechEvent>,
    reply_timeout: Duration,
}

impl Speech {
    pub fn new(dirs: Dirs, events: tokio::sync::mpsc::UnboundedSender<SpeechEvent>) -> Arc<Speech> {
        Speech::with_reply_timeout(dirs, events, REPLY_TIMEOUT)
    }

    pub fn with_reply_timeout(dirs: Dirs, events: tokio::sync::mpsc::UnboundedSender<SpeechEvent>, reply_timeout: Duration) -> Arc<Speech> {
        Arc::new(Speech { dirs, inner: Mutex::default(), generation: AtomicU64::new(0), counter: AtomicU64::new(0), events, reply_timeout })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn runtime(&self) -> PathBuf {
        let s = crate::settings::load(&self.dirs);
        if s.kokoro_runtime.is_empty() { default_runtime(&self.dirs) } else { PathBuf::from(s.kokoro_runtime) }
    }

    /// Ready to speak, or the `why_not` code.
    pub fn check(&self) -> Result<(), &'static str> {
        find_kokoro(&self.runtime())?;
        player(&self.dirs).map(|_| ()).ok_or("no_player")
    }

    /// Starts speaking `text`; returns its id. Stops what was being said.
    pub fn speak(self: &Arc<Self>, text: &str, language: &str, speed: f64) -> Result<String, crate::VoiceError> {
        let text: String = text.chars().take(MAX_TEXT).collect();
        let parts = sentences(&text);
        if parts.is_empty() {
            return Err(crate::VoiceError::invalid("nothing to say"));
        }
        let kokoro = find_kokoro(&self.runtime()).map_err(|w| crate::VoiceError::not_ready(crate::i18n::why_not(w)))?;
        let player = player(&self.dirs).ok_or_else(|| crate::VoiceError::not_ready(crate::i18n::why_not("no_player")))?;
        self.stop();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("speak-{}-{}", std::process::id(), self.counter.fetch_add(1, Ordering::SeqCst) + 1);
        self.lock().current = Some(id.clone());
        let (me, id2, language) = (self.clone(), id.clone(), kokoro_language(language));
        let speed = speed.clamp(0.5, 2.0);
        std::thread::Builder::new()
            .name("speak".into())
            .spawn(move || me.run(&id2, generation, &kokoro, &player, &parts, &language, speed))
            .map_err(|e| crate::VoiceError::new("failed", e.to_string()))?;
        Ok(id)
    }

    fn emit(&self, id: &str, state: &'static str, message: String) {
        let _ = self.events.send(SpeechEvent::State { id: id.into(), state, message });
    }

    fn current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    /// The end of speech `id`: `current` is cleared (when it still is this one) on every path,
    /// so Status never keeps saying "speaking" after an error.
    fn finish(&self, id: &str, generation: u64, state: &'static str, message: String) {
        let mut inner = self.lock();
        if inner.current.as_deref() == Some(id) {
            inner.current = None;
        }
        drop(inner);
        let state = if state == "done" && !self.current(generation) { "stopped" } else { state };
        self.emit(id, state, message);
    }

    fn run(&self, id: &str, generation: u64, kokoro: &Kokoro, player: &Path, parts: &[String], language: &str, speed: f64) {
        self.emit(id, "speaking", String::new());
        for part in parts {
            if !self.current(generation) {
                return self.finish(id, generation, "stopped", String::new());
            }
            let wav = match self.synthesize(kokoro, part, language, speed, generation) {
                Ok(w) => w,
                Err(_) if !self.current(generation) => return self.finish(id, generation, "stopped", String::new()),
                Err(e) => return self.finish(id, generation, "error", e),
            };
            if !self.current(generation) {
                let _ = std::fs::remove_file(&wav);
                return self.finish(id, generation, "stopped", String::new());
            }
            let result = self.play(player, &wav);
            let _ = std::fs::remove_file(&wav);
            if let Err(e) = result {
                if self.current(generation) {
                    return self.finish(id, generation, "error", e);
                }
            }
        }
        self.finish(id, generation, "done", String::new());
    }

    /// One sentence into a WAV in our private runtime folder, through the resident worker. The
    /// lock is only held to take and give back the worker's pipes, never while it works.
    fn synthesize(&self, kokoro: &Kokoro, text: &str, language: &str, speed: f64, generation: u64) -> Result<PathBuf, String> {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = crate::tempwav::private_folder(&self.dirs).map_err(|e| e.to_string())?;
        let wav = dir.join(format!("speech-{}-{}.wav", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let deadline = Instant::now() + self.reply_timeout;
        // The worker may still be busy with a stopped speech's last sentence: wait for it.
        let (mut io, pid) = loop {
            let mut inner = self.lock();
            inner.used = Some(Instant::now());
            if !inner.worker.as_mut().is_some_and(ChildGuard::alive) {
                inner.worker = None;
                inner.io = None;
                let (child, io) = start_worker(kokoro).map_err(|e| format!("Kokoro: {e}"))?;
                inner.worker = Some(child);
                inner.io = Some(io);
            }
            if let Some(io) = inner.io.take() {
                break (io, inner.worker.as_ref().and_then(ChildGuard::pid));
            }
            if Instant::now() >= deadline {
                // whoever has it is stuck: a new worker next round
                inner.worker = None;
                continue;
            }
            drop(inner);
            if !self.current(generation) {
                return Err("stopped".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // The worker reads `speed` as 1.0; it takes text, voice, language and output.
        let _ = speed;
        let request = serde_json::json!({"text": text, "voice": "martin", "language": language, "output": wav});
        let sent = writeln!(io.stdin, "{request}").and_then(|_| io.stdin.flush());
        // Once asked, the reply is read even after a Stop (until the deadline), so the worker
        // stays in step and can be used again; only Kokoro's own end or the deadline kill it.
        let line = match sent {
            Ok(()) => io.read_line(deadline),
            Err(_) => Err("Kokoro stopped"),
        };
        let mut inner = self.lock();
        let same = inner.worker.as_ref().and_then(ChildGuard::pid) == pid && pid.is_some();
        let line = match line {
            Ok(line) => {
                if same {
                    inner.io = Some(io);
                }
                line
            }
            Err(e) => {
                if same {
                    inner.worker = None; // killed and reaped (ChildGuard)
                }
                drop(inner);
                let _ = std::fs::remove_file(&wav);
                return Err(e.into());
            }
        };
        drop(inner);
        let reply: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
        if reply.get("code").and_then(serde_json::Value::as_i64) != Some(0) {
            let _ = std::fs::remove_file(&wav);
            return Err(reply.get("error").and_then(serde_json::Value::as_str).unwrap_or("Kokoro failed").chars().take(300).collect());
        }
        Ok(wav)
    }

    fn play(&self, player: &Path, wav: &Path) -> Result<(), String> {
        let mut command = Command::new(player);
        command.arg(wav).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let child = crate::procs::spawn(command).map_err(|e| format!("{}: {e}", player.display()))?;
        let pid = child.id();
        self.lock().player = Some(ChildGuard(Some(child)));
        // Wait without holding the lock, so Stop can kill it.
        loop {
            std::thread::sleep(Duration::from_millis(40));
            let mut inner = self.lock();
            let Some(guard) = inner.player.as_mut() else { return Ok(()) };
            if guard.pid() != Some(pid) {
                return Ok(());
            }
            if !guard.alive() {
                inner.player = None;
                return Ok(());
            }
        }
    }

    /// Stops the current speech (and its sound) at once.
    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let mut inner = self.lock();
        if let Some(mut p) = inner.player.take() {
            p.kill();
        }
        if let Some(id) = inner.current.take() {
            drop(inner);
            self.emit(&id, "stopped", String::new());
        }
    }

    pub fn speaking(&self) -> bool {
        self.lock().current.is_some()
    }

    /// Ends the worker (frees Kokoro's memory) when unused for `idle`.
    pub fn unload_if_idle(&self, idle: Duration) -> bool {
        let mut inner = self.lock();
        if inner.current.is_some() || inner.io.is_none() || inner.used.is_some_and(|t| t.elapsed() < idle) {
            return false;
        }
        inner.io = None;
        inner.worker.take().is_some()
    }

    pub fn shutdown(&self) {
        self.stop();
        let mut inner = self.lock();
        inner.io = None;
        inner.worker = None;
    }
}

fn player(dirs: &Dirs) -> Option<PathBuf> {
    ["pw-play", "paplay", "aplay"].iter().find_map(|p| dirs.which(p))
}

fn start_worker(kokoro: &Kokoro) -> std::io::Result<(ChildGuard, WorkerIo)> {
    let mut command = Command::new(&kokoro.python);
    command
        .arg(&kokoro.script)
        .arg("--worker")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(app) = kokoro.script.parent() {
        command.current_dir(app);
    }
    let mut child = crate::procs::spawn(command)?;
    let stdin = child.stdin.take().ok_or_else(|| std::io::Error::other("no stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?;
    Ok((ChildGuard(Some(child)), WorkerIo { stdin, stdout, pending: Vec::new() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::testdir::TestDir;

    #[test]
    fn sentences_split_and_clean() {
        let s = sentences("# Title\nParis is the capital. It has **2.1 million** people!\n- one\n```\ncode\n```\nDone");
        assert_eq!(s, vec!["Title.", "Paris is the capital.", "It has 2.1 million people!", "one.", "Done."]);
        assert!(sentences("  \n```\nx\n```").is_empty());
        let long = "a ".repeat(500);
        assert!(sentences(&long).iter().all(|p| p.chars().count() <= 400));
    }

    #[test]
    fn languages() {
        assert_eq!(kokoro_language("auto"), "de");
        assert_eq!(kokoro_language("en"), "en-us");
        assert_eq!(kokoro_language("fr"), "fr");
    }

    #[test]
    fn finds_kokoro_or_says_why() {
        let t = TestDir::new("kokoro");
        let rt = t.path().join("rt");
        assert_eq!(find_kokoro(&rt), Err("no_kokoro"));
        std::fs::create_dir_all(rt.join("app")).unwrap();
        std::fs::create_dir_all(rt.join("kokoro")).unwrap();
        std::fs::write(rt.join("app/kokoro_tts.py"), "").unwrap();
        let bin = t.script("python", "#!/bin/sh\n");
        std::fs::create_dir_all(rt.join("venv/bin")).unwrap();
        std::fs::copy(bin.join("python"), rt.join("venv/bin/python")).unwrap();
        assert_eq!(find_kokoro(&rt), Err("no_kokoro_model"));
        std::fs::write(rt.join("kokoro/kokoro-martin.onnx"), "x").unwrap();
        assert!(find_kokoro(&rt).is_ok());
    }

    /// A Kokoro runtime whose worker is `worker` (Python), and a fake player.
    fn fake_runtime(t: &TestDir, worker: &str) -> Dirs {
        let rt = t.path().join("home/.local/share/kindle-reader");
        for d in ["app", "kokoro", "venv/bin"] {
            std::fs::create_dir_all(rt.join(d)).unwrap();
        }
        std::fs::write(rt.join("kokoro/kokoro-martin.onnx"), "x").unwrap();
        std::fs::write(rt.join("app/kokoro_tts.py"), worker).unwrap();
        std::os::unix::fs::symlink(PathBuf::from("/usr/bin/python3"), rt.join("venv/bin/python")).unwrap();
        let bin = t.script("pw-play", "#!/bin/sh\nsleep 0.05\n");
        Dirs::rooted(t.path(), &format!("{}:/usr/bin:/bin", bin.display()))
    }

    fn end_state(rx: &mut tokio::sync::mpsc::UnboundedReceiver<SpeechEvent>, id: &str, within: Duration) -> (&'static str, String) {
        let start = Instant::now();
        while start.elapsed() < within {
            match rx.try_recv() {
                Ok(SpeechEvent::State { id: i, state, message }) if i == id && state != "speaking" => return (state, message),
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        ("none", String::new())
    }

    /// A worker that never answers: Stop, Status and a new Speak don't wait for it; the
    /// deadline kills it and the speech ends with an error that clears `speaking`.
    #[test]
    fn hung_worker_never_blocks() {
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let t = TestDir::new("speak-hung");
        let dirs = fake_runtime(&t, "import sys,time\nfor line in sys.stdin:\n time.sleep(600)\n");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let speech = Speech::with_reply_timeout(dirs, tx, Duration::from_millis(1500));
        let id = speech.speak("One.", "de", 1.0).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        speech.stop();
        assert!(!speech.speaking());
        assert!(started.elapsed() < Duration::from_millis(200), "stop waited for Kokoro");
        assert_eq!(end_state(&mut rx, &id, Duration::from_secs(1)).0, "stopped");
        // a new speech: the hung worker is killed at the deadline, the next one hangs too and
        // the speech ends with an error, `speaking` false again
        let id = speech.speak("Two.", "de", 1.0).unwrap();
        assert!(speech.speaking());
        let (state, message) = end_state(&mut rx, &id, Duration::from_secs(8));
        assert_eq!(state, "error", "{message}");
        assert!(!speech.speaking());
        speech.shutdown();
    }

    /// Kokoro answers with an error: the speech ends `error` and is no longer `speaking`.
    #[test]
    fn worker_error_clears_speaking() {
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let t = TestDir::new("speak-err");
        let dirs = fake_runtime(&t, "import sys,json\nfor line in sys.stdin:\n print(json.dumps({'code':1,'error':'bad voice'}),flush=True)\n");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let speech = Speech::new(dirs, tx);
        let id = speech.speak("One. Two.", "de", 1.0).unwrap();
        assert_eq!(end_state(&mut rx, &id, Duration::from_secs(5)), ("error", "bad voice".to_string()));
        assert!(!speech.speaking());
        // the worker answered, so it is kept and can be unloaded when idle
        assert!(speech.unload_if_idle(Duration::ZERO));
        speech.shutdown();
    }

    /// A fake worker (writes a short WAV per request) and a fake player: speak plays every
    /// sentence and ends `done`; stop ends it `stopped`.
    #[test]
    fn speaks_and_stops() {
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let t = TestDir::new("speak");
        let rt = t.path().join("home/.local/share/kindle-reader");
        for d in ["app", "kokoro", "venv/bin"] {
            std::fs::create_dir_all(rt.join(d)).unwrap();
        }
        std::fs::write(rt.join("kokoro/kokoro-martin.onnx"), "x").unwrap();
        std::fs::write(
            rt.join("app/kokoro_tts.py"),
            "import sys,json\nfor line in sys.stdin:\n r=json.loads(line)\n open(r['output'],'wb').write(b'RIFF')\n open(r['output']+'.log','a')\n print(json.dumps({'code':0,'error':''}),flush=True)\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(PathBuf::from("/usr/bin/python3"), rt.join("venv/bin/python")).unwrap();
        let bin = t.script("pw-play", "#!/bin/sh\necho \"$1\" >> \"$(dirname \"$0\")/played\"\nsleep 0.05\n");
        let dirs = Dirs::rooted(t.path(), &format!("{}:/usr/bin:/bin", bin.display()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let speech = Speech::new(dirs, tx);
        assert_eq!(speech.check(), Ok(()));
        let id = speech.speak("One. Two! Three?", "de", 1.0).unwrap();
        let mut states = Vec::new();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            match rx.try_recv() {
                Ok(SpeechEvent::State { id: i, state, .. }) if i == id => {
                    states.push(state);
                    if state != "speaking" {
                        break;
                    }
                }
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        assert_eq!(states, vec!["speaking", "done"]);
        let played = std::fs::read_to_string(bin.join("played")).unwrap();
        assert_eq!(played.lines().count(), 3);
        // The WAVs are gone again.
        assert!(played.lines().all(|p| !Path::new(p).exists()));
        // A long speech, stopped.
        let id = speech.speak(&"Word. ".repeat(50), "de", 1.0).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        speech.stop();
        let mut last = "";
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if let Ok(SpeechEvent::State { id: i, state, .. }) = rx.try_recv() {
                if i == id && state != "speaking" {
                    last = state;
                    break;
                }
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert_eq!(last, "stopped");
        assert!(!speech.speaking());
        assert!(speech.unload_if_idle(Duration::ZERO));
        speech.shutdown();
    }
}
