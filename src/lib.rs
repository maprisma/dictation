//! Maprisma Voice: one session D-Bus service (`com.maprisma.Voice1`) that records the
//! microphone, detects speech and turns it into text with a local or online engine, for every
//! Maprisma app. See docs/protocol.md for the API and docs/plan.md for the steps.

pub mod audio;
pub mod client;
pub mod core;
pub mod dbus;
pub mod engines;
pub mod http;
pub mod i18n;
pub mod models;
pub mod omarchy;
pub mod paths;
pub mod procs;
pub mod recorder;
pub mod service;
pub mod settings;
pub mod speech;
pub mod tempwav;
pub mod vad;
pub mod wake;

/// An error with a stable code (the D-Bus error name's last part, see docs/protocol.md) and a
/// message for people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceError {
    pub code: &'static str,
    pub message: String,
}

impl VoiceError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        VoiceError { code, message: message.into() }
    }
    pub fn disabled(m: impl Into<String>) -> Self {
        Self::new("disabled", m)
    }
    pub fn not_ready(m: impl Into<String>) -> Self {
        Self::new("not_ready", m)
    }
    pub fn invalid(m: impl Into<String>) -> Self {
        Self::new("invalid_args", m)
    }
    pub fn unknown_session(m: impl Into<String>) -> Self {
        Self::new("unknown_session", m)
    }
    pub fn not_supported(m: impl Into<String>) -> Self {
        Self::new("not_supported", m)
    }
    pub fn engine(m: impl Into<String>) -> Self {
        Self::new("engine_failed", m)
    }
    pub fn recorder(m: impl Into<String>) -> Self {
        Self::new("recorder_failed", m)
    }
    /// A broken connection (internal; the whisper engine retries once).
    pub fn io(m: impl Into<String>) -> Self {
        Self::new("io", m)
    }
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for VoiceError {}
