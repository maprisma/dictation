//! The overlay's settings, `$XDG_CONFIG_HOME/maprisma-dictation/settings.json` (a JSON object;
//! a missing or broken file or value falls back to the default). The overlay only reads them;
//! Maprisma Manager's Voice page (step 2) writes them, keeping keys it doesn't know.
//!
//! | key | default | meaning |
//! |---|---|---|
//! | `language` | `system` | `system`, `en`, `de`, `fr`, `es`, `it`, `pt`, `nl` or `pl` |
//! | `corners` | `desktop` | `desktop`, `slight`, `rounded` or `square` (pill radius clamped 12–26 unless square) |
//! | `motion` | `desktop` | `desktop` (follow the desktop's reduced-motion setting), `full` or `reduced` |
//! | `listen` | `true` | start listening when the overlay opens |
//! | `end` | `silence` | how a tapped (toggle) session ends: `silence` or `release` |
//! | `screenshot_key` | `true` | F12 saves a picture of the overlay to ~/Pictures/Maprisma Dictation/ |

use serde_json::Value;

const FILE_MAX: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub language: String,
    pub corners: String,
    pub motion: Motion,
    pub listen: bool,
    pub end_silence: bool,
    pub screenshot_key: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Desktop,
    Full,
    Reduced,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { language: "system".into(), corners: "desktop".into(), motion: Motion::Desktop, listen: true, end_silence: true, screenshot_key: true }
    }
}

pub fn parse(bytes: &[u8]) -> Settings {
    let mut s = Settings::default();
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(bytes) else { return s };
    let text = |k: &str| map.get(k).and_then(Value::as_str);
    if let Some(l) = text("language").filter(|l| *l == "system" || maprisma_voice::i18n::LANGUAGES.iter().any(|(c, _)| c == l)) {
        s.language = l.into();
    }
    if let Some(c) = text("corners") {
        s.corners = crate::corners::normalize(c).into();
    }
    s.motion = match text("motion") {
        Some("full") => Motion::Full,
        Some("reduced") => Motion::Reduced,
        _ => Motion::Desktop,
    };
    if let Some(b) = map.get("listen").and_then(Value::as_bool) {
        s.listen = b;
    }
    if let Some(e) = text("end") {
        s.end_silence = e != "release";
    }
    if let Some(b) = map.get("screenshot_key").and_then(Value::as_bool) {
        s.screenshot_key = b;
    }
    s
}

pub fn read() -> Settings {
    use std::io::Read;
    let mut bytes = vec![];
    match std::fs::File::open(crate::paths::settings_file()).and_then(|f| f.take(FILE_MAX + 1).read_to_end(&mut bytes)) {
        Ok(_) if bytes.len() as u64 <= FILE_MAX => parse(&bytes),
        _ => Settings::default(),
    }
}

/// Whether the desktop asks for less motion: Plasma's animation speed at 0 (kdeglobals
/// `[KDE] AnimationDurationFactor=0`), Hyprland's `animations:enabled` off, GNOME/GTK's
/// `enable-animations` off (also read by COSMIC apps through gsettings when present).
pub fn desktop_reduces_motion(desktop: crate::look::Desktop) -> bool {
    use crate::look::Desktop;
    match desktop {
        Desktop::Kde => {
            let file = crate::paths::config_root().join("kdeglobals");
            std::fs::read_to_string(file).is_ok_and(|t| kde_reduces_motion(&t))
        }
        Desktop::Hyprland => {
            let out = std::process::Command::new("hyprctl")
                .args(["-j", "getoption", "animations:enabled"])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            out.ok()
                .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
                .and_then(|v| v["int"].as_i64())
                .is_some_and(|v| v == 0)
        }
        Desktop::Cosmic | Desktop::Other => {
            let out = std::process::Command::new("gsettings")
                .args(["get", "org.gnome.desktop.interface", "enable-animations"])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            out.ok().is_some_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "false")
        }
    }
}

pub fn kde_reduces_motion(kdeglobals: &str) -> bool {
    let mut group = "";
    for line in kdeglobals.lines().map(str::trim) {
        if line.starts_with('[') && line.ends_with(']') {
            group = &line[1..line.len() - 1];
        } else if group == "KDE" {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == "AnimationDurationFactor" {
                    return v.trim().parse::<f32>().is_ok_and(|f| f <= 0.0);
                }
            }
        }
    }
    false
}

/// Reduced motion in effect.
pub fn reduced_motion(setting: Motion, desktop: impl FnOnce() -> bool) -> bool {
    match setting {
        Motion::Full => false,
        Motion::Reduced => true,
        Motion::Desktop => desktop(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_broken_values() {
        assert_eq!(parse(b""), Settings::default());
        assert_eq!(parse(b"[1]"), Settings::default());
        let s = parse(br#"{"language": "de", "corners": "square", "motion": "reduced", "listen": false, "end": "release", "screenshot_key": false, "other": 1}"#);
        assert_eq!(s, Settings { language: "de".into(), corners: "square".into(), motion: Motion::Reduced, listen: false, end_silence: false, screenshot_key: false });
        let s = parse(br#"{"language": "xx", "corners": "bogus", "motion": 3, "listen": "no"}"#);
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn reduced_motion_choices() {
        assert!(kde_reduces_motion("[General]\nx=1\n[KDE]\nAnimationDurationFactor=0\n"));
        assert!(!kde_reduces_motion("[KDE]\nAnimationDurationFactor=0.5\n"));
        assert!(!kde_reduces_motion("[General]\nAnimationDurationFactor=0\n"));
        assert!(!kde_reduces_motion(""));
        assert!(reduced_motion(Motion::Desktop, || true));
        assert!(!reduced_motion(Motion::Full, || true));
        assert!(reduced_motion(Motion::Reduced, || false));
    }
}
