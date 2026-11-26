//! Local Whisper through a resident whisper.cpp `whisper-server`: started once (lazily, or at
//! daemon start with `preload`), bound to 127.0.0.1 on a free port, kept running with the model
//! loaded, started again when it died or the model changed. Requests are
//! `POST /inference` multipart (`file`, `response_format=json`, `language`, `temperature=0`),
//! sent from memory, so no audio file is written.

use super::Engine;
use crate::VoiceError;
use crate::http;
use crate::procs::ChildGuard;
use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long loading a model may take.
pub const START_TIMEOUT: Duration = Duration::from_secs(120);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

struct Running {
    child: ChildGuard,
    port: u16,
    binary: PathBuf,
    model: PathBuf,
}

/// A resident whisper-server: the main one (GPU when the build has one), or the wake
/// recogniser's small one ([WhisperServer::cpu]).
#[derive(Default)]
pub struct WhisperServer {
    state: Mutex<Option<Running>>,
    /// When it was last asked for (ensure); for unloading when idle.
    used: Mutex<Option<Instant>>,
    /// Some(n): CPU only (`-ng`) with n threads, so it never competes for the GPU's memory.
    cpu_threads: Option<usize>,
}

impl WhisperServer {
    /// A CPU-only server with `threads` threads (the wake recogniser's).
    pub fn cpu(threads: usize) -> Self {
        WhisperServer { cpu_threads: Some(threads.max(1)), ..Default::default() }
    }

    /// The port of a running server for this binary and model; starts (or restarts) it when
    /// needed. Callers wait while it loads.
    pub fn ensure(&self, binary: &Path, model: &Path) -> Result<u16, VoiceError> {
        *self.used.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = state.as_mut() {
            if running.binary == binary && running.model == model && running.child.alive() {
                return Ok(running.port);
            }
        }
        if let Some(mut old) = state.take() {
            old.child.kill();
        }
        let running = start(binary, model, self.cpu_threads)?;
        let port = running.port;
        *state = Some(running);
        Ok(port)
    }

    /// (port, model) while it runs.
    pub fn running(&self) -> Option<(u16, PathBuf)> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let r = state.as_mut()?;
        if r.child.alive() { Some((r.port, r.model.clone())) } else { None }
    }

    /// Stops a running server not asked for in `idle` (frees the model's memory); true when it did.
    pub fn unload_if_idle(&self, idle: Duration) -> bool {
        let used = *self.used.lock().unwrap_or_else(|e| e.into_inner());
        if used.is_some_and(|t| t.elapsed() < idle) {
            return false;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match state.take() {
            Some(mut running) => {
                running.child.kill();
                true
            }
            None => false,
        }
    }

    pub fn stop(&self) {
        if let Some(mut running) = self.state.lock().unwrap_or_else(|e| e.into_inner()).take() {
            running.child.kill();
        }
    }
}

pub fn free_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
}

fn addr(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get() / 2).unwrap_or(4).clamp(1, 8)
}

fn start(binary: &Path, model: &Path, cpu_threads: Option<usize>) -> Result<Running, VoiceError> {
    match start_once(binary, model, cpu_threads, true) {
        Ok(running) => Ok(running),
        Err((err, stderr_log)) => {
            // Older whisper.cpp builds may not recognize `-sns` / `-nth`: fall back cleanly once.
            let lower = stderr_log.to_lowercase();
            if lower.contains("unknown") || lower.contains("unrecognized") || lower.contains("invalid") {
                start_once(binary, model, cpu_threads, false).map_err(|(e, _)| e)
            } else {
                Err(err)
            }
        }
    }
}

fn start_once(binary: &Path, model: &Path, cpu_threads: Option<usize>, suppress_flags: bool) -> Result<Running, (VoiceError, String)> {
    use std::process::{Command, Stdio};
    let port = free_port().map_err(|e| (VoiceError::engine(format!("no free port: {e}")), String::new()))?;
    let mut command = Command::new(binary);
    command.arg("-m").arg(model).args(["--host", "127.0.0.1", "--port", &port.to_string()]);
    if suppress_flags {
        command.args(["-sns", "-nth", "0.60"]);
    }
    command.args(["-t", &cpu_threads.unwrap_or_else(threads).to_string(), "-nt"]);
    if cpu_threads.is_some() {
        command.arg("-ng");
    }
    command
        // No static files; it runs in / so it can't serve anything from the cwd.
        .args(["--public", "/nonexistent-maprisma-voice-public"])
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = crate::procs::spawn(command).map_err(|e| (VoiceError::engine(format!("{}: {e}", binary.display())), String::new()))?;
    // Keep reading its log (a full pipe would block it); the last lines explain a failure.
    let tail: Arc<Mutex<VecDeque<String>>> = Arc::default();
    let mut reader = None;
    if let Some(stderr) = child.stderr.take() {
        let tail = tail.clone();
        reader = Some(std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut t = tail.lock().unwrap_or_else(|e| e.into_inner());
                if t.len() >= 20 {
                    t.pop_front();
                }
                t.push_back(line);
            }
        }));
    }
    let mut guard = ChildGuard(Some(child));
    let started = Instant::now();
    loop {
        if !guard.alive() {
            guard.kill();
            // It exited: its log ends, so the reader finishes.
            if let Some(r) = reader.take() {
                let _ = r.join();
            }
            let lines = tail.lock().unwrap_or_else(|e| e.into_inner());
            let all = lines.iter().cloned().collect::<Vec<_>>().join("\n");
            let last = lines.iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default();
            return Err((VoiceError::engine(format!("whisper-server stopped while starting: {last}")), all));
        }
        match http::request(addr(port), "GET", "/health", None, b"", Duration::from_secs(2)) {
            // 404: an older server without /health that already listens.
            Ok(r) if r.status == 200 || r.status == 404 => break,
            _ => {}
        }
        if started.elapsed() > START_TIMEOUT {
            return Err((VoiceError::engine("whisper-server did not get ready in time"), String::new()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(Running { child: guard, port, binary: binary.into(), model: model.into() })
}

/// Whisper's markers for "nothing said" / non-speech tags, and spaces.
pub fn clean_text(text: &str) -> String {
    let mut t = text.to_string();
    for marker in [
        "[BLANK_AUDIO]",
        "[ Silence ]",
        "[silence]",
        "(silence)",
        "[no speech]",
        "[MUSIC]",
        "[Music]",
        "[music]",
        "(music)",
        "(Music)",
        "[APPLAUSE]",
        "[Applause]",
        "[applause]",
        "(applause)",
        "(Applause)",
        "♪",
        "♫",
    ] {
        t = t.replace(marker, " ");
    }
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

const PHANTOM_PHRASES: &[&str] = &[
    "thank you",
    "thank you very much",
    "thanks for watching",
    "thank you for watching",
    "gracias",
    "muchas gracias",
    "vielen dank",
    "vielen dank furs zuschauen",
    "danke furs zuschauen",
    "untertitel",
    "untertitel der amara org community",
    "untertitel von der amara org community",
    "amara org",
    "subtitles by",
    "mbc",
    "you",
    "bye",
    "so",
];

fn is_phantom_hallucination(text: &str) -> bool {
    let norm = crate::wake::normalize(text);
    if norm.is_empty() {
        return true;
    }
    PHANTOM_PHRASES.contains(&norm.as_str())
        || norm.starts_with("subtitles by ")
        || norm.contains("amara org")
}

/// Parses a Whisper `json` or `verbose_json` response, optionally rejecting likely
/// silence/noise hallucinations using per-segment metadata.
pub fn parse_response(value: &serde_json::Value, filter: bool) -> String {
    if filter {
        if let Some(segments) = value.get("segments").and_then(|s| s.as_array()) {
            if segments.is_empty() {
                return String::new();
            }
            let mut kept = Vec::new();
            for seg in segments {
                let no_speech = seg.get("no_speech_prob").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let avg_logprob = seg.get("avg_logprob").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let compression = seg.get("compression_ratio").and_then(|v| v.as_f64());
                // OpenAI / whisper.cpp semantics: require both high no_speech_prob AND low avg_logprob
                // so confident speech after a pause is never dropped on no_speech_prob alone.
                if no_speech >= 0.60 && avg_logprob < -1.0 {
                    continue;
                }
                if compression.is_some_and(|c| c > 2.4) {
                    continue;
                }
                let text = clean_text(seg.get("text").and_then(|t| t.as_str()).unwrap_or(""));
                if text.is_empty() {
                    continue;
                }
                if no_speech >= 0.90 && text.split_whitespace().count() <= 3 {
                    continue;
                }
                if ((no_speech >= 0.25 && avg_logprob < -0.75) || no_speech >= 0.50) && is_phantom_hallucination(&text) {
                    continue;
                }
                kept.push(text);
            }
            return kept.join(" ");
        }
    }
    clean_text(value.get("text").and_then(|t| t.as_str()).unwrap_or(""))
}

/// One `/inference` request.
pub fn infer(port: u16, wav: &[u8], language: &str, hallucination_filter: bool) -> Result<String, VoiceError> {
    let format = if hallucination_filter { "verbose_json" } else { "json" };
    let (body, kind) = http::multipart(&[
        http::Part::File("file", "audio.wav", "audio/wav", wav),
        http::Part::Text("response_format", format),
        http::Part::Text("temperature", "0"),
        http::Part::Text("language", language),
    ]);
    let response = http::request(addr(port), "POST", "/inference", Some(&kind), &body, REQUEST_TIMEOUT).map_err(|e| VoiceError::io(format!("whisper-server: {e}")))?;
    let value: serde_json::Value = serde_json::from_slice(&response.body).map_err(|_| VoiceError::engine(format!("whisper-server answered {} without JSON", response.status)))?;
    if response.status == 200 && (value.get("text").and_then(|t| t.as_str()).is_some() || value.get("segments").and_then(|s| s.as_array()).is_some()) {
        return Ok(parse_response(&value, hallucination_filter));
    }
    let message = value.get("error").and_then(|e| e.as_str()).unwrap_or("unknown error");
    Err(VoiceError::engine(format!("whisper-server: {message}")))
}

pub struct WhisperEngine {
    pub server: Arc<WhisperServer>,
    pub binary: PathBuf,
    pub model: PathBuf,
    pub hallucination_filter: bool,
}

impl Engine for WhisperEngine {
    fn id(&self) -> &'static str {
        "whisper"
    }
    fn model(&self) -> String {
        self.model.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
    fn partials(&self) -> bool {
        true
    }
    fn transcribe(&self, pcm: &[i16], language: &str) -> Result<String, VoiceError> {
        let wav = crate::audio::wav_bytes(pcm);
        let port = self.server.ensure(&self.binary, &self.model)?;
        match infer(port, &wav, language, self.hallucination_filter) {
            // The connection broke: the server died. Start it again and try once more.
            Err(e) if e.code == "io" => {
                self.server.stop();
                let port = self.server.ensure(&self.binary, &self.model)?;
                infer(port, &wav, language, self.hallucination_filter)
            }
            other => other,
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::paths::testdir::TestDir;
    use std::io::{Read, Write};

    /// A fake whisper-server: a Python script on a port it is given (like the real one),
    /// answering /health and /inference, logging the form fields it got.
    pub const FAKE_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, os, http.server
args = sys.argv[1:]
port = int(args[args.index('--port') + 1]); model = args[args.index('-m') + 1]
log = os.environ.get('FAKE_LOG') or os.path.join(os.path.dirname(os.path.abspath(__file__)), 'server.log')
open(log, 'a').write('START %s %s\n' % (model, ' '.join(args)))
if 'broken' in model:
    sys.stderr.write('error: failed to load model\n'); sys.exit(3)
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        self.send_response(200 if self.path == '/health' else 404); self.end_headers(); self.wfile.write(b'{"status":"ok"}')
    def do_POST(self):
        body = self.rfile.read(int(self.headers['Content-Length']))
        fields = {}
        for part in body.split(b'--' + self.headers['Content-Type'].split('boundary=')[1].encode()):
            if b'name="' in part:
                name = part.split(b'name="')[1].split(b'"')[0].decode()
                value = part.split(b'\r\n\r\n', 1)[1][:-2]
                fields[name] = value[:4].decode('latin1') if name == 'file' else value.decode()
        open(log, 'a').write('POST %s %s\n' % (self.path, json.dumps(fields, sort_keys=True)))
        text = os.environ.get('FAKE_TEXT', ' hello world [BLANK_AUDIO]')
        self.send_response(200); self.send_header('Content-Type', 'application/json'); self.end_headers()
        self.wfile.write(json.dumps({'text': text}).encode())
http.server.HTTPServer(('127.0.0.1', port), H).serve_forever()
"#;

    #[test]
    fn cleaning() {
        assert_eq!(clean_text(" [BLANK_AUDIO] "), "");
        assert_eq!(clean_text("  Hallo   Welt.\n"), "Hallo Welt.");
        assert_eq!(clean_text("[MUSIC] ♪ Hello (applause) [Music] (music) [APPLAUSE]"), "Hello");
    }

    #[test]
    fn hallucination_response_filtering() {
        use serde_json::json;
        // High no_speech_prob WITH confident avg_logprob on real speech after a pause is KEPT (H1).
        let confident_after_pause = json!({
            "text": "Öffne bitte Firefox.",
            "segments": [{"text": " Öffne bitte Firefox.", "no_speech_prob": 0.72, "avg_logprob": -0.25, "compression_ratio": 1.0}]
        });
        assert_eq!(parse_response(&confident_after_pause, true), "Öffne bitte Firefox.");

        // High no_speech_prob WITH low avg_logprob is rejected (OpenAI/whisper.cpp semantics).
        let silence_hallucination = json!({
            "text": "Die ganze Zeit ist nicht so gut.",
            "segments": [{"text": " Die ganze Zeit ist nicht so gut.", "no_speech_prob": 0.65, "avg_logprob": -1.15, "compression_ratio": 1.1}]
        });
        assert_eq!(parse_response(&silence_hallucination, true), "");
        assert_eq!(parse_response(&silence_hallucination, false), "Die ganze Zeit ist nicht so gut.");

        // Extreme no_speech_prob (>= 0.90) on short <= 3 word blurb is rejected even with moderate logprob.
        let extreme_short = json!({
            "text": "Oh okay.",
            "segments": [{"text": " Oh okay.", "no_speech_prob": 0.93, "avg_logprob": -0.45, "compression_ratio": 1.0}]
        });
        assert_eq!(parse_response(&extreme_short, true), "");

        // High compression_ratio repetition loop is rejected.
        let loop_seg = json!({
            "text": "ja ja ja ja ja ja ja ja ja ja",
            "segments": [{"text": "ja ja ja ja ja ja ja ja ja ja", "no_speech_prob": 0.05, "avg_logprob": -0.4, "compression_ratio": 2.9}]
        });
        assert_eq!(parse_response(&loop_seg, true), "");

        // Empty segments array returns empty even if top-level text is non-empty.
        let empty_segs = json!({"text": "You", "segments": []});
        assert_eq!(parse_response(&empty_segs, true), "");

        // Phantom phrase with BOTH no_speech >= 0.25 AND avg_logprob < -0.75 is rejected.
        let phantom_both = json!({
            "text": "Thank you.",
            "segments": [{"text": " Thank you.", "no_speech_prob": 0.30, "avg_logprob": -0.85, "compression_ratio": 1.0}]
        });
        assert_eq!(parse_response(&phantom_both, true), "");

        // Phantom phrase with no_speech >= 0.50 is rejected.
        let phantom_high_nsp = json!({
            "text": "Gracias.",
            "segments": [{"text": " Gracias.", "no_speech_prob": 0.55, "avg_logprob": -0.35, "compression_ratio": 1.0}]
        });
        assert_eq!(parse_response(&phantom_high_nsp, true), "");

        // Clearly spoken "Thank you." / "Bye" with low no_speech_prob is KEPT even if avg_logprob < -0.75 (M1).
        let real_thank_you = json!({
            "text": "Thank you.",
            "segments": [{"text": " Thank you.", "no_speech_prob": 0.05, "avg_logprob": -0.85, "compression_ratio": 1.0}]
        });
        assert_eq!(parse_response(&real_thank_you, true), "Thank you.");

        // Mixed segments: bad segment dropped, real speech kept.
        let mixed = json!({
            "text": "Gracias. Guten Morgen.",
            "segments": [
                {"text": " Gracias.", "no_speech_prob": 0.65, "avg_logprob": -1.10, "compression_ratio": 1.0},
                {"text": " Guten Morgen.", "no_speech_prob": 0.03, "avg_logprob": -0.18, "compression_ratio": 1.1}
            ]
        });
        assert_eq!(parse_response(&mixed, true), "Guten Morgen.");

        // Missing segments (plain json format) falls back to clean_text.
        let plain = json!({"text": " Hallo [BLANK_AUDIO] Welt "});
        assert_eq!(parse_response(&plain, true), "Hallo Welt");
    }

    #[test]
    fn older_server_without_sns_flag_falls_back_automatically() {
        if !python() {
            eprintln!("python3 missing: skipped");
            return;
        }
        let t = TestDir::new("whisper-legacy");
        let legacy = FAKE_SERVER.replace(
            "args = sys.argv[1:]",
            "args = sys.argv[1:]\nif '-sns' in args:\n    sys.stderr.write('error: unknown argument: -sns\\n'); sys.exit(1)",
        );
        let bin = t.script("whisper-server", &legacy);
        let binary = bin.join("whisper-server");
        let model = t.path().join("ggml-base.bin");
        std::fs::write(&model, "x").unwrap();
        let server = Arc::new(WhisperServer::default());
        let engine = WhisperEngine { server: server.clone(), binary, model, hallucination_filter: true };
        assert_eq!(engine.transcribe(&[0; 1600], "en").unwrap(), "hello world");
        let log = std::fs::read_to_string(bin.join("server.log")).unwrap();
        assert!(!log.contains("-sns"));
        server.stop();
    }

    /// A one-shot fake HTTP server in a thread (Rust): checks the request /inference gets.
    #[test]
    fn inference_request() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 65536];
            loop {
                let n = s.read(&mut buf).unwrap();
                raw.extend_from_slice(&buf[..n]);
                if let Some(head_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&raw[..head_end]).to_lowercase();
                    let len: usize = head.lines().find_map(|l| l.strip_prefix("content-length:")).unwrap().trim().parse().unwrap();
                    if raw.len() >= head_end + 4 + len {
                        break;
                    }
                }
            }
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 26\r\n\r\n{\"text\": \" Guten Morgen.\"}").unwrap();
            String::from_utf8_lossy(&raw).into_owned()
        });
        let text = infer(port, &crate::audio::wav_bytes(&[1, 2, 3]), "de", true).unwrap();
        assert_eq!(text, "Guten Morgen.");
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /inference HTTP/1.1\r\n"));
        for field in ["name=\"response_format\"\r\n\r\nverbose_json", "name=\"temperature\"\r\n\r\n0", "name=\"language\"\r\n\r\nde", "filename=\"audio.wav\""] {
            assert!(request.contains(field), "{field}");
        }
        assert!(request.contains("RIFF"));
    }

    fn python() -> bool {
        std::process::Command::new("python3").arg("--version").output().is_ok()
    }

    #[test]
    fn resident_server_lifecycle() {
        if !python() {
            eprintln!("python3 missing: skipped");
            return;
        }
        let t = TestDir::new("whisper");
        let bin = t.script("whisper-server", FAKE_SERVER);
        let binary = bin.join("whisper-server");
        let model = t.path().join("ggml-base.bin");
        std::fs::write(&model, "x").unwrap();
        let server = Arc::new(WhisperServer::default());
        let engine = WhisperEngine { server: server.clone(), binary: binary.clone(), model: model.clone(), hallucination_filter: true };
        assert_eq!(engine.model(), "ggml-base.bin");
        assert!(server.running().is_none());
        assert_eq!(engine.transcribe(&[0; 1600], "auto").unwrap(), "hello world");
        let (port, _) = server.running().unwrap();
        // Stays resident: a second request uses the same server.
        assert_eq!(engine.transcribe(&[0; 1600], "en").unwrap(), "hello world");
        assert_eq!(server.running().unwrap().0, port);
        let log = std::fs::read_to_string(bin.join("server.log")).unwrap();
        assert_eq!(log.matches("START").count(), 1);
        assert!(log.contains("--host 127.0.0.1"));
        assert!(log.contains("-sns -nth 0.60"));
        assert!(log.contains(r#""language": "en""#) && log.contains(r#""file": "RIFF""#) && log.contains(r#""temperature": "0""#));
        // It died: the next request starts it again.
        let pid = server.state.lock().unwrap().as_ref().unwrap().child.pid().unwrap();
        // SAFETY: kill(2) on our own child.
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(engine.transcribe(&[0; 1600], "auto").unwrap(), "hello world");
        assert_eq!(std::fs::read_to_string(bin.join("server.log")).unwrap().matches("START").count(), 2);
        // Another model restarts it.
        let other = t.path().join("ggml-small.bin");
        std::fs::write(&other, "x").unwrap();
        server.ensure(&binary, &other).unwrap();
        assert_eq!(server.running().unwrap().1, other);
        // Idle unloading: not while recently used, then it frees the server.
        assert!(!server.unload_if_idle(Duration::from_secs(60)));
        assert!(server.running().is_some());
        std::thread::sleep(Duration::from_millis(30));
        assert!(server.unload_if_idle(Duration::from_millis(10)));
        assert!(server.running().is_none());
        assert!(!server.unload_if_idle(Duration::from_millis(10)));
        server.ensure(&binary, &other).unwrap();
        server.stop();
        assert!(server.running().is_none());
        // A model it can't load: the error says why.
        let broken = t.path().join("ggml-broken.bin");
        std::fs::write(&broken, "x").unwrap();
        let err = server.ensure(&binary, &broken).unwrap_err();
        assert!(err.message.contains("failed to load model"), "{}", err.message);
        // The wake recogniser's server: CPU only, 4 threads; the main one has no -ng.
        let cpu = WhisperServer::cpu(4);
        cpu.ensure(&binary, &model).unwrap();
        cpu.stop();
        let log = std::fs::read_to_string(bin.join("server.log")).unwrap();
        let starts: Vec<&str> = log.lines().filter(|l| l.starts_with("START")).collect();
        assert!(starts.last().unwrap().ends_with("-t 4 -nt -ng --public /nonexistent-maprisma-voice-public"), "{}", starts.last().unwrap());
        assert!(!starts[0].contains("-ng"));
    }
}
