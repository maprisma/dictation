//! The service's own messages in 8 languages (translations/*.json, compiled by
//! tools/gen-translations). The language is the `ui_language` setting (`system` = LC_ALL,
//! LC_MESSAGES, LANG); the daemon sets it again whenever it reads the settings.

use serde_json::Value;
use std::sync::{LazyLock, RwLock};

pub const LANGUAGES: [(&str, &str); 8] =
    [("en", "English"), ("de", "Deutsch"), ("fr", "Français"), ("es", "Español"), ("it", "Italiano"), ("pt", "Português"), ("nl", "Nederlands"), ("pl", "Polski")];

include!("translations_generated.rs");

static CATALOGS: LazyLock<Vec<Value>> = LazyLock::new(|| CATALOG_JSON.iter().map(|s| serde_json::from_str(s).unwrap()).collect());
static CURRENT: RwLock<usize> = RwLock::new(usize::MAX);

pub fn system_language(values: [&str; 3]) -> &'static str {
    let value = values.into_iter().find(|v| !v.is_empty()).unwrap_or("en");
    let code = value.split(['_', '.', '@', '-']).next().unwrap_or("").to_lowercase();
    LANGUAGES.iter().find(|(c, _)| *c == code).map_or("en", |(c, _)| *c)
}

fn system() -> &'static str {
    let values = ["LC_ALL", "LC_MESSAGES", "LANG"].map(|k| std::env::var(k).unwrap_or_default());
    system_language([&values[0], &values[1], &values[2]])
}

/// `system` or a code.
pub fn set_language(setting: &str) {
    let code = if setting == "system" { system() } else { setting };
    let index = LANGUAGES.iter().position(|(c, _)| *c == code).unwrap_or(0);
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = index;
}

fn index() -> usize {
    let i = *CURRENT.read().unwrap_or_else(|e| e.into_inner());
    if i == usize::MAX {
        set_language("system");
        return *CURRENT.read().unwrap_or_else(|e| e.into_inner());
    }
    i
}

pub fn tr_in(code_index: usize, key: &str) -> String {
    CATALOGS[code_index][key].as_str().unwrap_or(key).to_owned()
}

pub fn tr(key: &str) -> String {
    tr_in(index(), key)
}

pub fn trf(key: &str, values: &[&str]) -> String {
    let mut text = tr(key);
    for (i, v) in values.iter().enumerate() {
        text = text.replace(&format!("{{{i}}}"), v);
    }
    text
}

/// The message for a `why_not` code (docs/protocol.md).
pub fn why_not(code: &str) -> String {
    tr(why_not_key(code))
}

/// The catalog key for a `why_not` code.
pub fn why_not_key(code: &str) -> &'static str {
    match code {
        "" => "Ready",
        "disabled" => "Dictation is turned off.",
        "no_whisper_server" => "whisper-server was not found.",
        "no_model" => "No Whisper model was found.",
        "no_online_key" => "No online transcription API key (OPENAI_API_KEY or GROQ_API_KEY) was found.",
        "no_voxtype" => "Voxtype is not installed.",
        "no_superwhisper" => "Superwhisper is not installed.",
        "no_omarchy" => "Omarchy dictation is not installed.",
        "no_recorder" => "No audio recorder (pw-record, parecord or arecord) was found.",
        "no_kokoro" => "Kokoro (the Maprisma Narrator's voice) is not installed.",
        "no_kokoro_model" => "Kokoro has no voice model yet.",
        "no_player" => "No audio player (pw-play, paplay or aplay) was found.",
        _ => "No speech engine is set up yet.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_in_every_catalog() {
        let en = CATALOGS[0].as_object().unwrap();
        for (i, catalog) in CATALOGS.iter().enumerate() {
            let c = catalog.as_object().unwrap();
            assert_eq!(c.len(), en.len(), "{}", LANGUAGES[i].0);
            for (key, value) in en {
                let text = c.get(key).and_then(Value::as_str).unwrap_or_else(|| panic!("{} misses {key}", LANGUAGES[i].0));
                assert!(!text.is_empty());
                // Placeholders survive translation.
                for p in ["{0}", "{1}"] {
                    assert_eq!(value.as_str().unwrap().contains(p), text.contains(p), "{} {key}", LANGUAGES[i].0);
                }
            }
        }
    }

    #[test]
    fn every_message_used_is_a_key() {
        let en = CATALOGS[0].as_object().unwrap();
        for code in ["", "disabled", "no_engine", "no_whisper_server", "no_model", "no_online_key", "no_voxtype", "no_superwhisper", "no_omarchy", "no_recorder", "no_kokoro", "no_kokoro_model", "no_player"] {
            assert!(en.contains_key(why_not_key(code)), "{code}");
        }
        // Every tr()/trf() key in the sources is in the catalog.
        let re = regex::Regex::new(r#"\btrf?\("([^"]+)""#).unwrap();
        for file in ["src/main.rs", "src/client.rs", "src/service.rs"] {
            let text = std::fs::read_to_string(file).unwrap();
            for cap in re.captures_iter(&text) {
                assert!(en.contains_key(&cap[1]), "{file}: {}", &cap[1]);
            }
        }
    }

    #[test]
    fn languages_and_placeholders() {
        assert_eq!(system_language(["", "de_DE.UTF-8", "en_US"]), "de");
        assert_eq!(system_language(["", "", ""]), "en");
        assert_eq!(system_language(["C.UTF-8", "", ""]), "en");
        assert_eq!(tr_in(1, "Cancelled."), "Abgebrochen.");
        assert_eq!(tr_in(7, "missing key"), "missing key");
    }
}
