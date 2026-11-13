//! Speech engines and which one is used.
//!
//! - **whisper**: a resident whisper.cpp `whisper-server` (model stays loaded), [whisper];
//! - **online**: OpenAI whisper-1 or Groq whisper-large-v3-turbo, [online];
//! - **external**: Voxtype, Superwhisper, Omarchy's `omarchy-dictation`; these record and type
//!   the text themselves, [external].
//!
//! `engine = auto` picks whisper when a server and a model are found, else online when a key is
//! found, else the first external one found (Omarchy, Voxtype, Superwhisper).

pub mod external;
pub mod online;
pub mod whisper;

use crate::VoiceError;
use crate::models::ModelFile;
use crate::paths::Dirs;
use crate::recorder::Recorder;
use crate::settings::Settings;
use std::path::PathBuf;

pub trait Engine: Send + Sync {
    /// `whisper`, `online`, `voxtype`, `superwhisper` or `omarchy`.
    fn id(&self) -> &'static str;
    /// The model in use (file name, or the provider's model), if any.
    fn model(&self) -> String {
        String::new()
    }
    /// Records and types by itself: the service doesn't open the microphone, `Final` has no
    /// text and `typed = true`.
    fn external(&self) -> bool {
        false
    }
    /// Cheap enough for partial results while listening.
    fn partials(&self) -> bool {
        false
    }
    /// 16 kHz mono PCM to text; `language` is `auto` or a code. Blocking.
    fn transcribe(&self, _pcm: &[i16], _language: &str) -> Result<String, VoiceError> {
        Err(VoiceError::not_supported("this engine cannot transcribe audio"))
    }
    /// External engines: start recording. Blocking.
    fn start(&self) -> Result<(), VoiceError> {
        Ok(())
    }
    /// External engines: stop (they type the text) or cancel. Blocking.
    fn stop(&self, _cancel: bool) -> Result<(), VoiceError> {
        Ok(())
    }
}

/// What is installed (no keys, only whether one exists).
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub whisper_server: Option<PathBuf>,
    pub models: Vec<ModelFile>,
    pub model: Option<ModelFile>,
    pub online: Option<online::Provider>,
    pub omarchy: Option<PathBuf>,
    pub voxtype: Option<PathBuf>,
    pub superwhisper: Option<PathBuf>,
    pub recorder: Option<Recorder>,
}

/// whisper-server: the setting, then our own whisper.cpp build, then PATH.
pub fn find_whisper_server(dirs: &Dirs, setting: &str) -> Option<PathBuf> {
    if !setting.is_empty() {
        return crate::paths::is_executable(std::path::Path::new(setting)).then(|| PathBuf::from(setting));
    }
    let managed = dirs.managed_server();
    if crate::paths::is_executable(&managed) {
        return Some(managed);
    }
    dirs.which("whisper-server")
}

pub fn probe(dirs: &Dirs, settings: &Settings) -> Facts {
    let models = crate::models::discover(dirs);
    let model = crate::models::pick(&models, &settings.model).cloned();
    Facts {
        whisper_server: find_whisper_server(dirs, &settings.whisper_server),
        model,
        models,
        online: online::find(dirs),
        omarchy: dirs.which("omarchy-dictation"),
        voxtype: dirs.which("voxtype"),
        superwhisper: dirs.which("superwhisper"),
        recorder: crate::recorder::find(dirs),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    Whisper { server: PathBuf, model: ModelFile },
    Online(online::Provider),
    External(external::Kind, PathBuf),
}

impl Choice {
    pub fn id(&self) -> &'static str {
        match self {
            Choice::Whisper { .. } => "whisper",
            Choice::Online(_) => "online",
            Choice::External(kind, _) => kind.id(),
        }
    }
    pub fn model(&self) -> String {
        match self {
            Choice::Whisper { model, .. } => model.name.clone(),
            Choice::Online(p) => p.model.to_string(),
            Choice::External(..) => String::new(),
        }
    }
}

/// The engine for the `engine` setting, or why there is none (a `why_not` code, see
/// docs/protocol.md). Engines that need the service's own recording also need a recorder.
pub fn choose(setting: &str, facts: &Facts) -> Result<Choice, &'static str> {
    let whisper = || match (&facts.whisper_server, &facts.model) {
        (None, _) => Err("no_whisper_server"),
        (_, None) => Err("no_model"),
        (Some(server), Some(model)) => Ok(Choice::Whisper { server: server.clone(), model: model.clone() }),
    };
    let online = || facts.online.clone().map(Choice::Online).ok_or("no_online_key");
    let external = |kind: external::Kind| {
        let path = match kind {
            external::Kind::Omarchy => &facts.omarchy,
            external::Kind::Voxtype => &facts.voxtype,
            external::Kind::Superwhisper => &facts.superwhisper,
        };
        path.clone().map(|p| Choice::External(kind, p)).ok_or(kind.missing())
    };
    let needs_recorder = |choice: Choice| match choice {
        Choice::External(..) => Ok(choice),
        _ if facts.recorder.is_none() => Err("no_recorder"),
        _ => Ok(choice),
    };
    match setting {
        "whisper" => needs_recorder(whisper()?),
        "online" => needs_recorder(online()?),
        "omarchy" => external(external::Kind::Omarchy),
        "voxtype" => external(external::Kind::Voxtype),
        "superwhisper" => external(external::Kind::Superwhisper),
        _ => {
            if facts.recorder.is_some() {
                if let Ok(c) = whisper() {
                    return Ok(c);
                }
                if let Ok(c) = online() {
                    return Ok(c);
                }
            }
            for kind in [external::Kind::Omarchy, external::Kind::Voxtype, external::Kind::Superwhisper] {
                if let Ok(c) = external(kind) {
                    return Ok(c);
                }
            }
            if facts.recorder.is_none() && (whisper().is_ok() || online().is_ok()) { Err("no_recorder") } else { Err("no_engine") }
        }
    }
}

/// The engines that could be chosen now (for `Status`).
pub fn available(facts: &Facts) -> Vec<String> {
    ["whisper", "online", "omarchy", "voxtype", "superwhisper"]
        .into_iter()
        .filter(|e| choose(e, facts).is_ok())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> Facts {
        let model = ModelFile { name: "ggml-base.bin".into(), path: "/m/ggml-base.bin".into(), size: 1, managed: true };
        Facts {
            whisper_server: Some("/w/whisper-server".into()),
            models: vec![model.clone()],
            model: Some(model),
            online: Some(online::Provider::test("OpenAI")),
            omarchy: Some("/b/omarchy-dictation".into()),
            voxtype: Some("/b/voxtype".into()),
            superwhisper: Some("/b/superwhisper".into()),
            recorder: Some(Recorder::PwRecord("/b/pw-record".into())),
        }
    }

    #[test]
    fn auto_order() {
        let mut f = full();
        assert_eq!(choose("auto", &f).unwrap().id(), "whisper");
        f.model = None;
        assert_eq!(choose("auto", &f).unwrap().id(), "online");
        f.online = None;
        assert_eq!(choose("auto", &f).unwrap().id(), "omarchy");
        f.omarchy = None;
        assert_eq!(choose("auto", &f).unwrap().id(), "voxtype");
        f.voxtype = None;
        assert_eq!(choose("auto", &f).unwrap().id(), "superwhisper");
        f.superwhisper = None;
        assert_eq!(choose("auto", &f), Err("no_engine"));
        // Server missing, model there: still online first.
        let mut f = full();
        f.whisper_server = None;
        assert_eq!(choose("auto", &f).unwrap().id(), "online");
        // Without a recorder only external engines work.
        let mut f = full();
        f.recorder = None;
        assert_eq!(choose("auto", &f).unwrap().id(), "omarchy");
        f.omarchy = None;
        f.voxtype = None;
        f.superwhisper = None;
        assert_eq!(choose("auto", &f), Err("no_recorder"));
    }

    #[test]
    fn explicit_engines() {
        let f = full();
        for e in ["whisper", "online", "omarchy", "voxtype", "superwhisper"] {
            assert_eq!(choose(e, &f).unwrap().id(), e);
        }
        assert_eq!(available(&f).len(), 5);
        let mut f = full();
        f.whisper_server = None;
        assert_eq!(choose("whisper", &f), Err("no_whisper_server"));
        f.whisper_server = Some("/w".into());
        f.model = None;
        assert_eq!(choose("whisper", &f), Err("no_model"));
        let mut f = full();
        f.online = None;
        f.voxtype = None;
        assert_eq!(choose("online", &f), Err("no_online_key"));
        assert_eq!(choose("voxtype", &f), Err("no_voxtype"));
        let mut f = full();
        f.recorder = None;
        assert_eq!(choose("whisper", &f), Err("no_recorder"));
        assert_eq!(choose("voxtype", &f).unwrap().id(), "voxtype");
        assert_eq!(available(&f), ["omarchy", "voxtype", "superwhisper"]);
        // An explicit choice never falls back to another engine.
        let mut f = full();
        f.superwhisper = None;
        assert_eq!(choose("superwhisper", &f), Err("no_superwhisper"));
    }

    #[test]
    fn whisper_server_search() {
        use crate::paths::testdir::TestDir;
        let t = TestDir::new("server");
        let bin = t.script("whisper-server", "#!/bin/sh\n");
        let dirs = Dirs::rooted(t.path(), bin.to_str().unwrap());
        assert_eq!(find_whisper_server(&dirs, ""), Some(bin.join("whisper-server")));
        let managed = dirs.managed_server();
        std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
        std::fs::copy(bin.join("whisper-server"), &managed).unwrap();
        assert_eq!(find_whisper_server(&dirs, ""), Some(managed));
        assert_eq!(find_whisper_server(&dirs, "/nonexistent/whisper-server"), None);
        let explicit = bin.join("whisper-server");
        assert_eq!(find_whisper_server(&dirs, explicit.to_str().unwrap()), Some(explicit));
    }
}
