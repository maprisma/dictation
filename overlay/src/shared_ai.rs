//! The AI helper every Maprisma app uses, chosen in Maprisma Manager → AI:
//! `$XDG_CONFIG_HOME/maprisma-ai/settings.json`, a JSON object whose `helper` is `"auto"` or a
//! helper id (`^[A-Za-z0-9][A-Za-z0-9._:/@+-]{0,160}$`: catalog ids like `claude`, local models
//! like `pi:<provider>/<model>`). A missing or broken file or value is `"auto"`. The overlay only
//! reads it, each time it asks; a chosen helper that isn't installed or has no `ask` falls back to
//! auto ([crate::helper::choose]). (Adapted from maprisma-notifier's shared_ai.rs.)

use serde_json::Value;
use std::path::{Path, PathBuf};

pub const DIR: &str = "maprisma-ai";
pub const AUTO: &str = "auto";
const FILE_MAX: u64 = 64 * 1024;

pub fn file() -> PathBuf {
    crate::paths::config_root().join(DIR).join("settings.json")
}

/// `"auto"` or a helper id.
pub fn valid(text: &str) -> bool {
    let mut chars = text.chars();
    text == AUTO
        || (text.len() <= 161
            && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
            && chars.all(|c| c.is_ascii_alphanumeric() || "._:/@+-".contains(c)))
}

/// The file's `helper`; "auto" when it isn't a JSON object with a valid one.
pub fn parse(bytes: &[u8]) -> String {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|v| v.get("helper").and_then(Value::as_str).filter(|h| valid(h)).map(str::to_owned))
        .unwrap_or_else(|| AUTO.into())
}

pub fn choice_at(path: &Path) -> String {
    use std::io::Read;
    let mut bytes = vec![];
    match std::fs::File::open(path).and_then(|f| f.take(FILE_MAX + 1).read_to_end(&mut bytes)) {
        Ok(_) if bytes.len() as u64 <= FILE_MAX => parse(&bytes),
        _ => AUTO.into(),
    }
}

/// The helper choice for this session (read now).
pub fn choice() -> String {
    choice_at(&file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_is_under_the_config_root() {
        assert_eq!(file(), crate::paths::config_root().join("maprisma-ai/settings.json"));
        assert!(file().starts_with(crate::paths::test_roots()));
    }

    #[test]
    fn valid_values() {
        for ok in ["auto", "claude", "cursor-agent", "pi:ollama/qwen3:8b", "x@y+z", &"a".repeat(161)] {
            assert!(valid(ok), "{ok}");
        }
        for bad in ["", "-x", ".x", "../x", "a b", "a\n", &"a".repeat(162), "ä"] {
            assert!(!valid(bad), "{bad}");
        }
    }

    #[test]
    fn missing_broken_or_invalid_is_auto() {
        let dir = crate::paths::test_roots().join("shared-ai");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        assert_eq!(choice_at(&path), "auto");
        for text in ["", "{broken", "[]", "\"codex\"", "{}", r#"{"helper": 3}"#, r#"{"helper": "../x"}"#] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(choice_at(&path), "auto", "{text}");
        }
        std::fs::write(&path, r#"{"helper": "codex", "other": 1}"#).unwrap();
        assert_eq!(choice_at(&path), "codex");
        std::fs::write(&path, format!(r#"{{"helper": "claude", "pad": "{}"}}"#, "x".repeat(FILE_MAX as usize))).unwrap();
        assert_eq!(choice_at(&path), "auto", "too big: not read");
    }
}
