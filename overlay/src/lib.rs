//! Maprisma Dictation: the top-centre voice and typing overlay (step 4, docs/overlay.md). It
//! listens through maprisma-voice (com.maprisma.Voice1), asks the shared AI helper and shows the
//! answer, with the aura ring around the pill.

pub mod actions;
pub mod aura;
pub mod bar;
pub mod corners;
pub mod helper;
pub mod instance;
pub mod last;
pub mod look;
pub mod markdown;
pub mod paths;
pub mod settings;
pub mod shared_ai;
pub mod shot;
pub mod state;
pub mod ui;
pub mod voice;

#[cfg(test)]
mod tests {
    /// Every text the overlay shows is in all 8 catalogs (translations/*.json; the voice
    /// service's test checks the catalogs agree with each other).
    #[test]
    fn every_overlay_text_is_translated() {
        let en: serde_json::Map<String, serde_json::Value> = serde_json::from_str(include_str!("../../translations/en.json")).unwrap();
        let re = regex::Regex::new(r#"\btrf?\("([^"]+)""#).unwrap();
        let note = regex::Regex::new(r#"note = Some\("([^"]+)""#).unwrap();
        let mut seen = 0;
        // Keys chosen at run time: `trf(if light { "…" } else { "…" }`, the block-list reasons, the weather words.
        let either = regex::Regex::new(r#"trf\(if \w+ \{ "([^"]+)" \} else \{ "([^"]+)" \}"#).unwrap();
        let why = regex::Regex::new(r#"pub const WHY_\w+: &str = "([^"]+)""#).unwrap();
        let mut keys = vec![];
        for file in ["src/ui.rs", "src/helper.rs", "src/voice.rs", "src/state.rs", "src/actions/mod.rs", "src/actions/exec.rs", "src/actions/policy.rs", "src/bar.rs", "src/tray/main.rs", "src/applet/main.rs"] {
            let text = std::fs::read_to_string(file).unwrap();
            for cap in re.captures_iter(&text).chain(note.captures_iter(&text)).chain(why.captures_iter(&text)) {
                keys.push((file, cap[1].to_string()));
            }
            for cap in either.captures_iter(&text) {
                keys.push((file, cap[1].to_string()));
                keys.push((file, cap[2].to_string()));
            }
        }
        for code in [0, 1, 2, 3, 45, 51, 56, 61, 71, 80, 85, 95, -1] {
            keys.push(("src/actions/weather.rs", crate::actions::weather::condition(code).to_string()));
        }
        for (file, key) in keys {
            assert!(en.contains_key(&key), "{file}: {key}");
            seen += 1;
        }
        assert!(seen > 20, "{seen}");
        for (i, (code, _)) in maprisma_voice::i18n::LANGUAGES.iter().enumerate().skip(1) {
            assert_ne!(maprisma_voice::i18n::tr_in(i, "Speak or type…"), "Speak or type…", "{code}");
        }
    }
}
