//! Online transcription (Maprisma's `_findOnlineKey` and its curl call, ported): the key comes
//! from `OPENAI_API_KEY`, then `GROQ_API_KEY`, then an `OPENAI_API_KEY=` line in `~/.codex/.env`.
//! The key never appears on a command line, in a log or in `Status`: curl reads the
//! `Authorization` header from its config on stdin (`-K -`). The audio goes to curl as a 0600
//! temporary WAV ([crate::tempwav]) that is removed right after.

use super::Engine;
use crate::VoiceError;
use crate::paths::Dirs;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

pub const OPENAI_URL: &str = "https://api.openai.com/v1/audio/transcriptions";
pub const GROQ_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";

#[derive(Clone, PartialEq)]
pub struct Provider {
    pub name: &'static str,
    pub endpoint: String,
    pub model: &'static str,
    /// Where the key came from (`OPENAI_API_KEY`, `GROQ_API_KEY`, `~/.codex/.env`).
    pub source: &'static str,
    key: String,
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Provider({}, {}, {})", self.name, self.model, self.source)
    }
}

impl Provider {
    fn openai(key: &str, source: &'static str) -> Self {
        Provider { name: "OpenAI", endpoint: OPENAI_URL.into(), model: "whisper-1", source, key: key.into() }
    }
    fn groq(key: &str) -> Self {
        Provider { name: "Groq", endpoint: GROQ_URL.into(), model: "whisper-large-v3-turbo", source: "GROQ_API_KEY", key: key.into() }
    }
    #[cfg(test)]
    pub fn test(name: &'static str) -> Self {
        Provider { name, endpoint: OPENAI_URL.into(), model: "whisper-1", source: "test", key: "k".into() }
    }
}

/// A key that is safe inside curl's quoted config value.
fn usable(key: &str) -> bool {
    !key.is_empty() && !key.chars().any(|c| c.is_control() || c == '"' || c == '\\' || c.is_whitespace())
}

pub fn find(dirs: &Dirs) -> Option<Provider> {
    if let Some(key) = dirs.key("OPENAI_API_KEY").map(str::trim).filter(|k| usable(k)) {
        return Some(Provider::openai(key, "OPENAI_API_KEY"));
    }
    if let Some(key) = dirs.key("GROQ_API_KEY").map(str::trim).filter(|k| usable(k)) {
        return Some(Provider::groq(key));
    }
    let text = std::fs::read_to_string(dirs.home.join(".codex/.env")).ok()?;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(value) = line.strip_prefix("OPENAI_API_KEY=") {
            let value = value.trim();
            let value = value.strip_prefix(['"', '\'']).unwrap_or(value);
            let value = value.strip_suffix(['"', '\'']).unwrap_or(value);
            if usable(value) {
                return Some(Provider::openai(value, "~/.codex/.env"));
            }
        }
    }
    None
}

/// curl's arguments: no key here.
pub fn curl_args(provider: &Provider, wav: &Path, language: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-sS", "--max-time", "60", "-K", "-", "-X", "POST"].map(OsString::from).to_vec();
    args.push(provider.endpoint.clone().into());
    let mut file = OsString::from("file=@");
    file.push(wav);
    args.extend(["-F".into(), file, "-F".into(), format!("model={}", provider.model).into(), "-F".into(), "response_format=json".into(), "-F".into(), "temperature=0".into()]);
    if language != "auto" {
        args.extend(["-F".into(), format!("language={language}").into()]);
    }
    args
}

/// curl's config on stdin: the header with the key.
pub fn curl_config(provider: &Provider) -> String {
    format!("header = \"Authorization: Bearer {}\"\n", provider.key)
}

/// The `text` of a transcription answer, or the API's error message.
pub fn parse_answer(body: &[u8]) -> Result<String, VoiceError> {
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| VoiceError::engine("the online service sent no JSON"))?;
    if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
        return Ok(super::whisper::clean_text(text));
    }
    let message = value.pointer("/error/message").and_then(|m| m.as_str()).unwrap_or("unknown error");
    Err(VoiceError::engine(format!("online transcription failed: {message}")))
}

pub struct OnlineEngine {
    pub dirs: Dirs,
    pub provider: Provider,
}

impl Engine for OnlineEngine {
    fn id(&self) -> &'static str {
        "online"
    }
    fn model(&self) -> String {
        format!("{} {}", self.provider.name, self.provider.model)
    }
    fn transcribe(&self, pcm: &[i16], language: &str) -> Result<String, VoiceError> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let curl = self.dirs.which("curl").ok_or_else(|| VoiceError::not_ready("curl is not installed"))?;
        let wav = crate::tempwav::TempWav::create(&self.dirs, &crate::audio::wav_bytes(pcm)).map_err(|e| VoiceError::engine(format!("temporary audio file: {e}")))?;
        let mut command = Command::new(curl);
        command.args(curl_args(&self.provider, wav.path(), language)).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = crate::procs::spawn(command).map_err(|e| VoiceError::engine(format!("curl: {e}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(curl_config(&self.provider).as_bytes());
        }
        let output = wait_output(child, Duration::from_secs(75))?;
        drop(wav);
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(VoiceError::engine(format!("online transcription failed: {detail}")));
        }
        parse_answer(&output.stdout)
    }
}

fn wait_output(child: std::process::Child, timeout: Duration) -> Result<std::process::Output, VoiceError> {
    let (tx, rx) = std::sync::mpsc::channel();
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(VoiceError::engine(format!("curl: {e}"))),
        Err(_) => {
            // SAFETY: kill(2) on our own child.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            Err(VoiceError::engine("online transcription timed out"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::testdir::TestDir;

    #[test]
    fn key_order() {
        let t = TestDir::new("keys");
        let dirs = Dirs::rooted(t.path(), "/nowhere");
        assert_eq!(find(&dirs), None);
        std::fs::create_dir_all(dirs.home.join(".codex")).unwrap();
        std::fs::write(dirs.home.join(".codex/.env"), "# x\nOTHER=1\n  OPENAI_API_KEY=\"sk-codex\"  \n").unwrap();
        let p = find(&dirs).unwrap();
        assert_eq!((p.name, p.source, p.key.as_str()), ("OpenAI", "~/.codex/.env", "sk-codex"));
        let p = find(&dirs.clone().with_key("GROQ_API_KEY", "gsk-1")).unwrap();
        assert_eq!((p.name, p.model, p.endpoint.as_str()), ("Groq", "whisper-large-v3-turbo", GROQ_URL));
        let p = find(&dirs.clone().with_key("GROQ_API_KEY", "gsk-1").with_key("OPENAI_API_KEY", "sk-env")).unwrap();
        assert_eq!((p.name, p.source, p.model), ("OpenAI", "OPENAI_API_KEY", "whisper-1"));
        // Empty or unsafe keys don't count.
        std::fs::write(dirs.home.join(".codex/.env"), "OPENAI_API_KEY=''\n").unwrap();
        assert_eq!(find(&dirs.clone().with_key("OPENAI_API_KEY", "  ")), None);
        assert_eq!(find(&dirs.clone().with_key("OPENAI_API_KEY", "a\"b")), None);
        assert!(!format!("{:?}", Provider::openai("sk-secret", "x")).contains("sk-secret"));
    }

    #[test]
    fn key_only_on_stdin() {
        let p = Provider::openai("sk-secret", "OPENAI_API_KEY");
        let args = curl_args(&p, Path::new("/run/x/a.wav"), "de");
        let joined: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(!joined.iter().any(|a| a.contains("sk-secret")));
        assert!(joined.contains(&"file=@/run/x/a.wav".to_string()));
        assert!(joined.contains(&"model=whisper-1".to_string()));
        assert!(joined.contains(&"language=de".to_string()));
        assert!(!curl_args(&p, Path::new("/a.wav"), "auto").iter().any(|a| a.to_string_lossy().starts_with("language=")));
        assert_eq!(curl_config(&p), "header = \"Authorization: Bearer sk-secret\"\n");
    }

    #[test]
    fn answers() {
        assert_eq!(parse_answer(br#"{"text": " Hallo Welt. "}"#).unwrap(), "Hallo Welt.");
        assert!(parse_answer(br#"{"error": {"message": "bad key"}}"#).unwrap_err().message.contains("bad key"));
        assert!(parse_answer(b"<html>").is_err());
    }

    /// The whole call with a fake curl: the key arrives on stdin only, the temporary WAV is
    /// private and gone afterwards.
    #[test]
    fn transcribe_with_fake_curl() {
        let t = TestDir::new("online");
        let log = t.path().join("log");
        let script = format!(
            "#!/bin/sh\nlog={}\necho \"ARGS $*\" >> $log\ncat >> $log\nfor a; do case $a in file=@*) f=${{a#file=@}}; stat -c 'MODE %a' \"$f\" >> $log; head -c 4 \"$f\" >> $log; echo >> $log;; esac; done\necho '{{\"text\": \"hello online\"}}'\n",
            log.display()
        );
        let bin = t.script("curl", &script);
        let dirs = Dirs::rooted(t.path(), bin.to_str().unwrap()).with_key("OPENAI_API_KEY", "sk-test-123");
        let engine = OnlineEngine { provider: find(&dirs).unwrap(), dirs: dirs.clone() };
        let text = engine.transcribe(&crate::audio::synth::tone(300, 200.0, 5000.0), "en").unwrap();
        assert_eq!(text, "hello online");
        let log = std::fs::read_to_string(&log).unwrap();
        let args_line = log.lines().find(|l| l.starts_with("ARGS")).unwrap();
        assert!(!args_line.contains("sk-test-123"));
        assert!(log.contains("header = \"Authorization: Bearer sk-test-123\""));
        assert!(log.contains("MODE 600") && log.contains("RIFF"));
        let runtime = dirs.runtime_dir.unwrap().join("maprisma-voice");
        assert_eq!(std::fs::read_dir(&runtime).unwrap().count(), 0);
    }
}
