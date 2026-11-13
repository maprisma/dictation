//! The real [Backend]: settings (read again for every session and `Status`), engine choice,
//! the resident whisper-server and the recorder.

use crate::VoiceError;
use crate::core::{Backend, Prepared, SessionRequest};
use crate::engines::{self, Choice, Engine, Facts};
use crate::paths::Dirs;
use crate::recorder::Capture;
use crate::settings::{self, Settings};
use serde_json::{Map, Value};
use std::sync::Arc;

pub struct RealBackend {
    pub dirs: Dirs,
    pub server: Arc<engines::whisper::WhisperServer>,
}

impl RealBackend {
    pub fn new(dirs: Dirs) -> Self {
        RealBackend { dirs, server: Arc::default() }
    }

    fn read(&self) -> (Settings, Facts) {
        let s = settings::load(&self.dirs);
        crate::i18n::set_language(&s.ui_language);
        let facts = engines::probe(&self.dirs, &s);
        (s, facts)
    }

    /// With `preload`, start whisper-server now so the first dictation doesn't wait for the
    /// model. Blocking.
    pub fn preload(&self) {
        let (s, facts) = self.read();
        if s.enabled && s.preload {
            if let Ok(Choice::Whisper { server, model }) = engines::choose(&s.engine, &facts) {
                if let Err(e) = self.server.ensure(&server, &model.path) {
                    eprintln!("maprisma-voice: preload: {e}");
                }
            }
        }
    }

    pub fn stop(&self) {
        self.server.stop();
    }
}

/// The engine object for a choice.
pub fn make_engine(dirs: &Dirs, server: &Arc<engines::whisper::WhisperServer>, choice: Choice) -> Arc<dyn Engine> {
    match choice {
        Choice::Whisper { server: binary, model } => Arc::new(engines::whisper::WhisperEngine { server: server.clone(), binary, model: model.path }),
        Choice::Online(provider) => Arc::new(engines::online::OnlineEngine { dirs: dirs.clone(), provider }),
        Choice::External(kind, program) => Arc::new(engines::external::ExternalEngine { kind, program }),
    }
}

impl Backend for RealBackend {
    fn prepare(&self, request: &SessionRequest) -> Result<Prepared, VoiceError> {
        let (s, facts) = self.read();
        if !s.enabled {
            return Err(VoiceError::disabled(crate::i18n::why_not("disabled")));
        }
        let choice = engines::choose(&s.engine, &facts).map_err(|why| VoiceError::not_ready(crate::i18n::why_not(why)))?;
        let language = request.language.clone().unwrap_or(s.language.clone());
        Ok(Prepared { engine: make_engine(&self.dirs, &self.server, choice), language, microphone: s.microphone })
    }

    fn capture(&self, microphone: &str) -> Result<Capture, VoiceError> {
        let recorder = crate::recorder::find(&self.dirs).ok_or_else(|| VoiceError::not_ready(crate::i18n::why_not("no_recorder")))?;
        crate::recorder::start(&recorder, microphone).map_err(|e| VoiceError::recorder(format!("{}: {e}", recorder.name())))
    }

    fn status(&self) -> Map<String, Value> {
        let (s, facts) = self.read();
        let mut map = status_map(&s, &facts);
        let running = self.server.running();
        map.insert("server_running".into(), running.is_some().into());
        map.insert("server_port".into(), Value::from(running.map_or(0, |(p, _)| p as u64)));
        map
    }
}

/// `Status`'s engine fields from the settings and what is installed (no key ever).
pub fn status_map(s: &Settings, facts: &Facts) -> Map<String, Value> {
    let choice = engines::choose(&s.engine, facts);
    let why = if !s.enabled { "disabled" } else { choice.as_ref().err().copied().unwrap_or("") };
    let mut m = Map::new();
    m.insert("version".into(), env!("CARGO_PKG_VERSION").into());
    m.insert("enabled".into(), s.enabled.into());
    m.insert("ready".into(), why.is_empty().into());
    m.insert("why_not".into(), why.into());
    m.insert("message".into(), crate::i18n::why_not(why).into());
    m.insert("engine_setting".into(), s.engine.clone().into());
    m.insert("engine".into(), choice.as_ref().map(|c| c.id()).unwrap_or("").into());
    m.insert("model".into(), choice.as_ref().map(|c| c.model()).unwrap_or_default().into());
    m.insert("language".into(), s.language.clone().into());
    m.insert("microphone".into(), s.microphone.clone().into());
    m.insert("typed_by_engine".into(), matches!(choice, Ok(Choice::External(..))).into());
    m.insert("engines".into(), engines::available(facts).into());
    m.insert("recorder".into(), facts.recorder.as_ref().map(|r| r.name()).unwrap_or("").into());
    m.insert("whisper_server".into(), facts.whisper_server.as_ref().map(|p| p.display().to_string()).unwrap_or_default().into());
    m.insert("models".into(), facts.models.iter().map(|m| m.name.clone()).collect::<Vec<_>>().into());
    m.insert("online_provider".into(), facts.online.as_ref().map(|p| p.name).unwrap_or("").into());
    m
}

/// `transcribe FILE`: the configured (or given) engine on a file, without the daemon or the
/// microphone. Starts its own whisper-server and stops it afterwards.
pub fn transcribe_file(dirs: &Dirs, pcm: &[i16], engine: Option<&str>, model: Option<&str>, language: Option<&str>) -> Result<(String, String, String), VoiceError> {
    let mut s = settings::load(dirs);
    crate::i18n::set_language(&s.ui_language);
    if let Some(m) = model {
        s.model = m.into();
    }
    let facts = engines::probe(dirs, &s);
    // A file needs no recorder.
    let facts = Facts { recorder: Some(crate::recorder::Recorder::Arecord("/".into())), ..facts };
    let choice = engines::choose(engine.unwrap_or(&s.engine), &facts).map_err(|why| VoiceError::not_ready(crate::i18n::why_not(why)))?;
    if matches!(choice, Choice::External(..)) {
        return Err(VoiceError::not_supported(crate::i18n::tr("This engine types the text itself and cannot transcribe a file.")));
    }
    let server: Arc<engines::whisper::WhisperServer> = Arc::default();
    let engine = make_engine(dirs, &server, choice);
    let result = engine.transcribe(pcm, language.unwrap_or(&s.language));
    server.stop();
    Ok((result?, engine.id().into(), engine.model()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::testdir::TestDir;

    #[test]
    fn status_never_shows_a_key_and_explains() {
        let t = TestDir::new("status");
        let dirs = Dirs::rooted(t.path(), "/nowhere").with_key("OPENAI_API_KEY", "sk-very-secret");
        let backend = RealBackend::new(dirs.clone());
        let m = backend.status();
        // A key but no recorder on this PATH.
        assert_eq!(m["ready"], false);
        assert_eq!(m["why_not"], "no_recorder");
        assert_eq!(m["online_provider"], "OpenAI");
        assert!(!serde_json::to_string(&m).unwrap().contains("sk-very-secret"));
        settings::set(&dirs, "enabled", Some(false.into())).unwrap();
        assert_eq!(backend.status()["why_not"], "disabled");
        let req = SessionRequest { app_id: "x".into(), mode: crate::core::Mode::Dictate, end: crate::core::End::Release, language: None };
        assert_eq!(backend.prepare(&req).err().unwrap().code, "disabled");
        settings::set(&dirs, "enabled", None).unwrap();
        assert_eq!(backend.prepare(&req).err().unwrap().code, "not_ready");
    }

    #[test]
    fn prepare_uses_settings_and_overrides() {
        let t = TestDir::new("prepare");
        let bin = t.script("pw-record", "#!/bin/sh\n");
        t.script("voxtype", "#!/bin/sh\n");
        let dirs = Dirs::rooted(t.path(), bin.to_str().unwrap()).with_key("GROQ_API_KEY", "gsk");
        let backend = RealBackend::new(dirs.clone());
        settings::set(&dirs, "language", Some("de".into())).unwrap();
        let mut req = SessionRequest { app_id: "x".into(), mode: crate::core::Mode::Dictate, end: crate::core::End::Release, language: None };
        let p = backend.prepare(&req).unwrap();
        assert_eq!((p.engine.id(), p.language.as_str()), ("online", "de"));
        req.language = Some("en".into());
        assert_eq!(backend.prepare(&req).unwrap().language, "en");
        // Changed settings take effect at the next session.
        settings::set(&dirs, "engine", Some("voxtype".into())).unwrap();
        assert_eq!(backend.prepare(&req).unwrap().engine.id(), "voxtype");
        assert_eq!(backend.status()["typed_by_engine"], true);
    }
}
