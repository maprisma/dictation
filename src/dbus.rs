//! The session D-Bus service `com.maprisma.Voice1` (docs/protocol.md).

use crate::VoiceError;
use crate::core::{Core, End, Event, Mode, SessionRequest};
use std::collections::HashMap;
use std::sync::Arc;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

pub const NAME: &str = "com.maprisma.Voice1";
pub const PATH: &str = "/com/maprisma/Voice1";
pub const INTERFACE: &str = "com.maprisma.Voice1";

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "com.maprisma.Voice1.Error")]
pub enum DbusError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Disabled(String),
    NotReady(String),
    InvalidArgs(String),
    UnknownSession(String),
    NotSupported(String),
    EngineFailed(String),
    RecorderFailed(String),
    Failed(String),
}

impl From<VoiceError> for DbusError {
    fn from(e: VoiceError) -> Self {
        match e.code {
            "disabled" => DbusError::Disabled(e.message),
            "not_ready" => DbusError::NotReady(e.message),
            "invalid_args" => DbusError::InvalidArgs(e.message),
            "unknown_session" => DbusError::UnknownSession(e.message),
            "not_supported" => DbusError::NotSupported(e.message),
            "engine_failed" | "io" => DbusError::EngineFailed(e.message),
            "recorder_failed" => DbusError::RecorderFailed(e.message),
            _ => DbusError::Failed(e.message),
        }
    }
}

fn text_option<'a>(options: &'a HashMap<String, OwnedValue>, key: &str) -> Result<Option<&'a str>, VoiceError> {
    match options.get(key).map(|v| &**v) {
        None => Ok(None),
        Some(Value::Str(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(VoiceError::invalid(format!("option {key} must be a string"))),
    }
}

/// `StartSession`'s arguments as a request.
pub fn parse_request(app_id: &str, mode: &str, options: &HashMap<String, OwnedValue>) -> Result<SessionRequest, VoiceError> {
    let app_id = app_id.trim();
    if app_id.is_empty() || app_id.len() > 255 || app_id.chars().any(char::is_control) {
        return Err(VoiceError::invalid("app_id must be a short name"));
    }
    let mode = Mode::parse(mode).ok_or_else(|| VoiceError::invalid("mode must be dictate or assistant"))?;
    for key in options.keys() {
        if !["end", "language"].contains(&key.as_str()) {
            return Err(VoiceError::invalid(format!("unknown option {key}")));
        }
    }
    let end = match text_option(options, "end")? {
        None | Some("release") => End::Release,
        Some("silence") => End::Silence,
        Some(_) => return Err(VoiceError::invalid("end must be release or silence")),
    };
    let language = match text_option(options, "language")? {
        None | Some("") => None,
        Some(l) if crate::settings::valid_language(l) => Some(l.to_string()),
        Some(_) => return Err(VoiceError::invalid("language must be auto or a language code")),
    };
    Ok(SessionRequest { app_id: app_id.into(), mode, end, language })
}

pub fn json_to_value(v: &serde_json::Value) -> Value<'static> {
    match v {
        serde_json::Value::Bool(b) => Value::from(*b),
        serde_json::Value::Number(n) if n.is_u64() => Value::from(n.as_u64().unwrap()),
        serde_json::Value::Number(n) if n.is_i64() => Value::from(n.as_i64().unwrap()),
        serde_json::Value::Number(n) => Value::from(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => Value::from(s.clone()),
        serde_json::Value::Array(a) => Value::from(a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect::<Vec<String>>()),
        _ => Value::from(""),
    }
}

pub fn value_to_json(v: &Value<'_>) -> serde_json::Value {
    match v {
        Value::Bool(b) => (*b).into(),
        Value::U64(n) => (*n).into(),
        Value::I64(n) => (*n).into(),
        Value::U32(n) => (*n).into(),
        Value::I32(n) => (*n).into(),
        Value::F64(n) => (*n).into(),
        Value::Str(s) => s.as_str().into(),
        Value::Array(a) => a.iter().map(value_to_json).collect::<Vec<_>>().into(),
        Value::Value(inner) => value_to_json(inner),
        other => other.to_string().into(),
    }
}

pub struct Voice {
    pub core: Arc<Core>,
    pub speech: Arc<crate::speech::Speech>,
}

#[zbus::interface(name = "com.maprisma.Voice1")]
impl Voice {
    async fn start_session(
        &self,
        app_id: String,
        mode: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> Result<String, DbusError> {
        let request = parse_request(&app_id, &mode, &options)?;
        // Listening and speaking never overlap: a new session stops the voice at once.
        self.speech.stop();
        let id = self.core.start_session(request).await?;
        // Cancel the session when its client leaves the bus (crashed app: no open microphone).
        if let Some(sender) = header.sender().map(|s| s.to_owned()) {
            let (core, id, connection) = (self.core.clone(), id.clone(), connection.clone());
            tokio::spawn(async move { watch_client(connection, core, id, sender.into()).await });
        }
        Ok(id)
    }

    async fn finish(&self, session_id: String) -> Result<(), DbusError> {
        Ok(self.core.finish(&session_id)?)
    }

    async fn cancel(&self, session_id: String) -> Result<(), DbusError> {
        Ok(self.core.cancel(&session_id)?)
    }

    async fn status(&self) -> HashMap<String, OwnedValue> {
        let (core, speech) = (self.core.clone(), self.speech.clone());
        let map = tokio::task::spawn_blocking(move || {
            let mut map = core.status();
            let why = speech.check().err().unwrap_or("");
            map.insert("speech_ready".into(), why.is_empty().into());
            map.insert("speech_why_not".into(), why.into());
            map.insert("speech_message".into(), if why.is_empty() { String::new() } else { crate::i18n::why_not(why) }.into());
            map.insert("speaking".into(), speech.speaking().into());
            map
        })
        .await
        .unwrap_or_default();
        map.iter().filter_map(|(k, v)| OwnedValue::try_from(json_to_value(v)).ok().map(|v| (k.clone(), v))).collect()
    }

    /// Reads `text` aloud with Kokoro (options: `language`); returns its id. `Speaking` signals
    /// follow. Stops what was being said; refused while a session listens.
    async fn speak(&self, text: String, options: HashMap<String, OwnedValue>) -> Result<String, DbusError> {
        if self.core.status().get("state").and_then(serde_json::Value::as_str) == Some("listening") {
            return Err(DbusError::Failed("busy: listening".into()));
        }
        let language = text_option(&options, "language")?.unwrap_or("auto").to_string();
        if !crate::settings::valid_language(&language) {
            return Err(DbusError::InvalidArgs("language".into()));
        }
        let speech = self.speech.clone();
        Ok(tokio::task::spawn_blocking(move || speech.speak(&text, &language, 1.0)).await.map_err(|e| DbusError::Failed(e.to_string()))??)
    }

    async fn stop_speaking(&self) -> Result<(), DbusError> {
        self.speech.stop();
        Ok(())
    }

    /// A speech's state: `speaking`, `done`, `stopped` or `error` (with a message).
    #[zbus(signal)]
    pub async fn speaking(emitter: &SignalEmitter<'_>, speak_id: &str, state: &str, message: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn partial(emitter: &SignalEmitter<'_>, session_id: &str, text: &str) -> zbus::Result<()>;

    #[zbus(signal, name = "Final")]
    pub async fn final_text(emitter: &SignalEmitter<'_>, session_id: &str, text: &str, details: HashMap<&str, Value<'_>>) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn level(emitter: &SignalEmitter<'_>, session_id: &str, rms: f64) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn state(emitter: &SignalEmitter<'_>, session_id: &str, state: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn error(emitter: &SignalEmitter<'_>, session_id: &str, code: &str, message: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn closed(emitter: &SignalEmitter<'_>, session_id: &str, reason: &str) -> zbus::Result<()>;
}

async fn watch_client(connection: zbus::Connection, core: Arc<Core>, id: String, sender: zbus::names::UniqueName<'static>) {
    use futures_util::StreamExt;
    let Ok(proxy) = zbus::fdo::DBusProxy::new(&connection).await else { return };
    let Ok(mut changes) = proxy.receive_name_owner_changed_with_args(&[(0, sender.as_str())]).await else { return };
    // Gone already (between the call and the watch)?
    if !proxy.name_has_owner(sender.as_ref().into()).await.unwrap_or(true) {
        core.client_gone(&id);
        return;
    }
    let mut closed = std::pin::pin!(wait_closed(core.clone(), id.clone()));
    loop {
        tokio::select! {
            change = changes.next() => {
                let Some(change) = change else { return };
                if let Ok(args) = change.args() {
                    if args.new_owner().is_none() {
                        core.client_gone(&id);
                        return;
                    }
                }
            }
            _ = &mut closed => return,
        }
    }
}

/// Resolves once the session is gone (so the watch ends with it).
async fn wait_closed(core: Arc<Core>, id: String) {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if !core.is_open(&id) {
            return;
        }
    }
}

async fn forward(connection: zbus::Connection, mut events: tokio::sync::mpsc::UnboundedReceiver<Event>) {
    let Ok(emitter) = SignalEmitter::new(&connection, PATH) else { return };
    while let Some(event) = events.recv().await {
        let result = match &event {
            Event::Partial { id, text } => Voice::partial(&emitter, id, text).await,
            Event::Final { id, text, details } => {
                let mut d: HashMap<&str, Value<'_>> = HashMap::new();
                d.insert("typed", details.typed.into());
                d.insert("engine", details.engine.as_str().into());
                d.insert("model", details.model.as_str().into());
                d.insert("language", details.language.as_str().into());
                d.insert("audio_ms", details.audio_ms.into());
                d.insert("elapsed_ms", details.elapsed_ms.into());
                Voice::final_text(&emitter, id, text, d).await
            }
            Event::Level { id, rms } => Voice::level(&emitter, id, *rms).await,
            Event::State { id, state } => Voice::state(&emitter, id, state).await,
            Event::Error { id, code, message } => Voice::error(&emitter, id, code, message).await,
            Event::Closed { id, reason } => Voice::closed(&emitter, id, reason).await,
        };
        if let Err(e) = result {
            eprintln!("maprisma-voice: signal: {e}");
        }
    }
}

/// `maprisma-voice daemon`: owns the name until SIGTERM/SIGINT or the bus goes away.
pub async fn run_daemon(dirs: crate::paths::Dirs) -> Result<(), String> {
    let (speech_tx, mut speech_rx) = tokio::sync::mpsc::unbounded_channel();
    let speech = crate::speech::Speech::new(dirs.clone(), speech_tx);
    let backend = Arc::new(crate::service::RealBackend::new(dirs));
    let (core, events) = Core::new(backend.clone(), crate::vad::VadConfig::default());
    let connection = zbus::connection::Builder::session()
        .map_err(|e| e.to_string())?
        .serve_at(PATH, Voice { core: core.clone(), speech: speech.clone() })
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| format!("no session bus: {e}"))?;
    match connection.request_name_with_flags(NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into()).await {
        Ok(zbus::fdo::RequestNameReply::PrimaryOwner) | Ok(zbus::fdo::RequestNameReply::AlreadyOwner) => {}
        Ok(_) | Err(zbus::Error::NameTaken) => return Err(format!("{NAME} is already running")),
        Err(e) => return Err(e.to_string()),
    }
    tokio::spawn(forward(connection.clone(), events));
    let speech_connection = connection.clone();
    tokio::spawn(async move {
        let Ok(emitter) = SignalEmitter::new(&speech_connection, PATH) else { return };
        while let Some(crate::speech::SpeechEvent::State { id, state, message }) = speech_rx.recv().await {
            let _ = Voice::speaking(&emitter, &id, state, &message).await;
        }
    });
    let preload = backend.clone();
    tokio::task::spawn_blocking(move || preload.preload());
    // Idle unloading: checked every minute, never while a session lives.
    let (idle_backend, idle_core, idle_speech) = (backend.clone(), core.clone(), speech.clone());
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tick.tick().await;
            if idle_core.status().get("sessions").and_then(serde_json::Value::as_u64) == Some(0) {
                let b = idle_backend.clone();
                let _ = tokio::task::spawn_blocking(move || b.unload_if_idle()).await;
                let minutes = crate::settings::load(&idle_backend.dirs).unload_minutes;
                if minutes > 0 {
                    idle_speech.unload_if_idle(std::time::Duration::from_secs(60 * u64::from(minutes)));
                }
            }
        }
    });
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|e| e.to_string())?;
    let mut int = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).map_err(|e| e.to_string())?;
    // The bus going away ends us too (the connection's executor stops).
    let bus = zbus::fdo::DBusProxy::new(&connection).await.map_err(|e| e.to_string())?;
    let mut lost = bus.receive_name_lost().await.map_err(|e| e.to_string())?;
    use futures_util::StreamExt;
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
        _ = lost.next() => {}
    }
    // Every session first: an external recorder (Voxtype …) must not go on after we're gone.
    if !core.shutdown().await {
        eprintln!("maprisma-voice: a session did not stop in time");
    }
    let b = backend.clone();
    let _ = tokio::task::spawn_blocking(move || b.stop()).await;
    speech.shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(pairs: &[(&str, Value<'static>)]) -> HashMap<String, OwnedValue> {
        pairs.iter().map(|(k, v)| (k.to_string(), OwnedValue::try_from(v.clone()).unwrap())).collect()
    }

    #[test]
    fn requests() {
        let r = parse_request("launcher", "dictate", &opts(&[])).unwrap();
        assert_eq!((r.mode, r.end, r.language), (Mode::Dictate, End::Release, None));
        let r = parse_request("theme", "assistant", &opts(&[("end", "silence".into()), ("language", "de".into())])).unwrap();
        assert_eq!((r.mode, r.end, r.language.as_deref()), (Mode::Assistant, End::Silence, Some("de")));
        for (app, mode, o) in [
            ("", "dictate", opts(&[])),
            ("x", "talk", opts(&[])),
            ("x", "dictate", opts(&[("end", "later".into())])),
            ("x", "dictate", opts(&[("end", Value::from(3u32))])),
            ("x", "dictate", opts(&[("language", "German".into())])),
            ("x", "dictate", opts(&[("volume", "1".into())])),
        ] {
            assert_eq!(parse_request(app, mode, &o).unwrap_err().code, "invalid_args", "{app} {mode}");
        }
    }

    #[test]
    fn values_both_ways() {
        let j = serde_json::json!({"a": true, "b": 3, "c": "x", "d": ["p", "q"], "e": 1.5});
        for (_, v) in j.as_object().unwrap() {
            assert_eq!(&value_to_json(&json_to_value(v)), v);
        }
    }
}
