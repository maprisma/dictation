//! Maprisma Dictation: the top-centre voice and typing overlay (step 4, docs/overlay.md). It
//! listens through maprisma-voice (com.maprisma.Voice1), asks the shared AI helper and shows the
//! answer, with the aura ring around the pill.

pub mod actions;
pub mod aura;
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
        for file in ["src/ui.rs", "src/helper.rs", "src/voice.rs", "src/state.rs"] {
            let text = std::fs::read_to_string(file).unwrap();
            for cap in re.captures_iter(&text).chain(note.captures_iter(&text)) {
                assert!(en.contains_key(&cap[1]), "{file}: {}", &cap[1]);
                seen += 1;
            }
        }
        assert!(seen > 20, "{seen}");
        for (i, (code, _)) in maprisma_voice::i18n::LANGUAGES.iter().enumerate().skip(1) {
            assert_ne!(maprisma_voice::i18n::tr_in(i, "Speak or type…"), "Speak or type…", "{code}");
        }
    }
}
