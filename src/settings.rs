//! `${XDG_CONFIG_HOME:-~/.config}/maprisma-voice/settings.json`. Every key is optional; a missing
//! or invalid value is its default. Writers change only their own keys and keep every other key
//! ([set]); the folder is 0700 and the file 0600. The daemon reads it again for every
//! `StartSession` and `Status`, so a change takes effect without a restart.

use crate::paths::Dirs;
use serde_json::{Map, Value};

pub const ENGINES: [&str; 6] = ["auto", "whisper", "online", "voxtype", "superwhisper", "omarchy"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    /// One of [ENGINES].
    pub engine: String,
    /// A model file name (`ggml-small.bin`) or catalog id (`small`); empty picks one.
    pub model: String,
    /// The spoken language: `auto` or a Whisper language code (`de`, `en`, …).
    pub language: String,
    /// A PipeWire node name, or `default`.
    pub microphone: String,
    /// The whisper-server program; empty searches for it.
    pub whisper_server: String,
    /// Start whisper-server (and load the model) when the daemon starts, not at the first session.
    pub preload: bool,
    /// The language of the service's own messages: `system` or one of the 8 codes.
    pub ui_language: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: true,
            engine: "auto".into(),
            model: String::new(),
            language: "auto".into(),
            microphone: "default".into(),
            whisper_server: String::new(),
            preload: false,
            ui_language: "system".into(),
        }
    }
}

/// `auto`, or 2–3 lowercase ASCII letters (Whisper's codes, plus `yue`/`haw`), or `auto`.
pub fn valid_language(code: &str) -> bool {
    code == "auto" || ((2..=3).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_lowercase()))
}

/// A PipeWire node name: printable, no spaces at the ends, not too long.
pub fn valid_microphone(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && name.trim() == name && !name.chars().any(char::is_control) && !name.starts_with('-')
}

pub fn parse(value: &Value) -> Settings {
    let d = Settings::default();
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    let engine = text("engine").filter(|e| ENGINES.contains(e)).unwrap_or(&d.engine);
    let model = text("model").map(str::trim).filter(|m| crate::models::valid_setting(m)).unwrap_or("");
    let language = text("language").map(str::trim).filter(|l| valid_language(l)).unwrap_or(&d.language);
    let microphone = text("microphone").filter(|m| valid_microphone(m)).unwrap_or(&d.microphone);
    let server = text("whisper_server").filter(|s| s.starts_with('/') && !s.chars().any(char::is_control)).unwrap_or("");
    let ui = text("ui_language").filter(|l| *l == "system" || crate::i18n::LANGUAGES.iter().any(|(c, _)| c == l)).unwrap_or(&d.ui_language);
    Settings {
        enabled: value.get("enabled").and_then(Value::as_bool).unwrap_or(d.enabled),
        engine: engine.into(),
        model: model.into(),
        language: language.into(),
        microphone: microphone.into(),
        whisper_server: server.into(),
        preload: value.get("preload").and_then(Value::as_bool).unwrap_or(d.preload),
        ui_language: ui.into(),
    }
}

pub fn read_value(dirs: &Dirs) -> Value {
    std::fs::read(dirs.settings_file())
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()))
}

pub fn load(dirs: &Dirs) -> Settings {
    parse(&read_value(dirs))
}

/// Checks a value for one of our keys before it is written (the CLI's `set`).
pub fn check(key: &str, value: &Value) -> Result<(), String> {
    let ok = match key {
        "enabled" | "preload" => value.is_boolean(),
        "engine" => value.as_str().is_some_and(|e| ENGINES.contains(&e)),
        "model" => value.as_str().is_some_and(|m| m.is_empty() || crate::models::valid_setting(m)),
        "language" => value.as_str().is_some_and(valid_language),
        "microphone" => value.as_str().is_some_and(valid_microphone),
        "whisper_server" => value.as_str().is_some_and(|s| s.is_empty() || s.starts_with('/')),
        "ui_language" => value.as_str().is_some_and(|l| l == "system" || crate::i18n::LANGUAGES.iter().any(|(c, _)| *c == l)),
        _ => return Err(format!("unknown setting {key}")),
    };
    if ok { Ok(()) } else { Err(format!("invalid value for {key}")) }
}

/// Sets (Some) or removes (None) one key; every other key stays as it is (read–modify–write,
/// atomic, 0700 folder, 0600 file). A missing or broken file starts as `{}`.
pub fn set(dirs: &Dirs, key: &str, value: Option<Value>) -> std::io::Result<()> {
    let path = dirs.settings_file();
    let mut object = match read_value(dirs) {
        Value::Object(o) => o,
        _ => Map::new(),
    };
    match value {
        Some(v) => {
            object.insert(key.into(), v);
        }
        None => {
            object.remove(key);
        }
    }
    crate::paths::private_dir(path.parent().unwrap())?;
    let mut bytes = serde_json::to_vec_pretty(&Value::Object(object))?;
    bytes.push(b'\n');
    crate::paths::write_atomic(&path, &bytes, 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::testdir::TestDir;
    use serde_json::json;

    #[test]
    fn defaults_for_missing_and_broken() {
        assert_eq!(parse(&json!({})), Settings::default());
        assert_eq!(parse(&json!("x")), Settings::default());
        let broken = json!({"enabled": "yes", "engine": "magic", "model": "../evil.bin", "language": "German",
            "microphone": " x", "whisper_server": "relative/path", "preload": 1, "ui_language": "xx"});
        assert_eq!(parse(&broken), Settings::default());
        assert_eq!(parse(&json!({"microphone": "-x"})).microphone, "default");
        assert_eq!(parse(&json!({"model": "ggml-x.bin; rm"})).model, "");
    }

    #[test]
    fn values() {
        let s = parse(&json!({"enabled": false, "engine": "online", "model": "ggml-small.bin", "language": "de",
            "microphone": "alsa_input.usb-Mic", "whisper_server": "/opt/w/whisper-server", "preload": true,
            "ui_language": "de", "window_size": {"w": 1}}));
        assert!(!s.enabled && s.preload);
        assert_eq!(s.engine, "online");
        assert_eq!(s.model, "ggml-small.bin");
        assert_eq!(s.language, "de");
        assert_eq!(s.microphone, "alsa_input.usb-Mic");
        assert_eq!(s.whisper_server, "/opt/w/whisper-server");
        assert_eq!(s.ui_language, "de");
        assert_eq!(parse(&json!({"model": "turbo"})).model, "turbo");
        for e in ENGINES {
            assert_eq!(parse(&json!({ "engine": e })).engine, e);
        }
    }

    #[test]
    fn languages() {
        for ok in ["auto", "de", "en", "yue", "haw"] {
            assert!(valid_language(ok), "{ok}");
        }
        for bad in ["", "D", "de-DE", "german", "d", "../"] {
            assert!(!valid_language(bad), "{bad}");
        }
    }

    #[test]
    fn set_keeps_other_keys_and_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let t = TestDir::new("settings");
        let dirs = Dirs::rooted(t.path(), "/nowhere");
        let path = dirs.settings_file();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"x-manager": {"a": 1}, "engine": "online"}"#).unwrap();
        set(&dirs, "language", Some("de".into())).unwrap();
        set(&dirs, "engine", None).unwrap();
        let v = read_value(&dirs);
        assert_eq!(v["x-manager"]["a"], 1);
        assert_eq!(v["language"], "de");
        assert!(v.get("engine").is_none());
        assert_eq!(load(&dirs).language, "de");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        // A broken file is replaced by a valid one with just the new key.
        std::fs::write(&path, "{broken").unwrap();
        set(&dirs, "enabled", Some(false.into())).unwrap();
        assert_eq!(read_value(&dirs), json!({"enabled": false}));
    }

    #[test]
    fn checks() {
        assert!(check("engine", &json!("whisper")).is_ok());
        assert!(check("engine", &json!("x")).is_err());
        assert!(check("enabled", &json!(true)).is_ok());
        assert!(check("enabled", &json!("true")).is_err());
        assert!(check("model", &json!("")).is_ok());
        assert!(check("model", &json!("a/b.bin")).is_err());
        assert!(check("nope", &json!(1)).is_err());
    }
}
