//! The font for an answer's prose: the desktop's interface font.
//!
//! libcosmic draws every text in COSMIC's interface font (`com.system76.CosmicTk` →
//! `interface_font`), on every desktop, since the home folder is shared. That is right on COSMIC;
//! on KDE the interface font is kdeglobals' `font`, and on Hyprland (which has none of its own)
//! it is GTK's `org.gnome.desktop.interface font-name`, as GTK apps there show it. A family
//! that isn't installed is left alone (libcosmic's font then).

use crate::look::Desktop;
use std::sync::OnceLock;
use std::time::Duration;

static PROSE: OnceLock<Option<&'static str>> = OnceLock::new();

/// "'Noto Sans Bold 11'" (gsettings) → "Noto Sans Bold": the family as Pango names it, without
/// the size.
pub fn from_gsettings(value: &str) -> Option<String> {
    let v = value.trim().trim_matches('\'').trim();
    let family = match v.rsplit_once(char::is_whitespace) {
        Some((family, size)) if size.parse::<f32>().is_ok() => family.trim(),
        _ => v,
    };
    clean(family)
}

/// kdeglobals' `font=Noto Sans,10,-1,5,400,0,0,0,0,0` → "Noto Sans".
pub fn from_kdeglobals(text: &str) -> Option<String> {
    let mut general = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            general = line == "[General]";
        } else if general && let Some(value) = line.strip_prefix("font=") {
            return clean(value.split(',').next().unwrap_or_default());
        }
    }
    None
}

fn clean(family: &str) -> Option<String> {
    let f = family.trim();
    (!f.is_empty() && f.len() <= 100 && !f.chars().any(char::is_control)).then(|| f.to_string())
}

/// What a program prints, killed after a second (gsettings, fc-list).
fn output(argv: &[&str]) -> Option<String> {
    let mut child = std::process::Command::new(argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_secs(1) => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    use std::io::Read;
    child.stdout.take()?.take(64 * 1024).read_to_string(&mut out).ok()?;
    Some(out)
}

fn installed(family: &str) -> bool {
    output(&["fc-list", family, "family"]).is_some_and(|o| !o.trim().is_empty())
}

/// The desktop's interface font family, when it isn't libcosmic's own (COSMIC: None).
pub fn desktop_family(desktop: Desktop) -> Option<String> {
    if cfg!(test) {
        return None;
    }
    let family = match desktop {
        Desktop::Hyprland => from_gsettings(&output(&["gsettings", "get", "org.gnome.desktop.interface", "font-name"])?),
        Desktop::Kde => from_kdeglobals(&std::fs::read_to_string(crate::paths::home().join(".config/kdeglobals")).ok()?),
        _ => None,
    }?;
    installed(&family).then_some(family)
}

/// Looks the font up once, on a thread of its own (the overlay opens without waiting for it).
pub fn load(desktop: Desktop) {
    let _ = std::thread::Builder::new().name("fonts".into()).spawn(move || {
        let family = desktop_family(desktop).map(|f| &*Box::leak(f.into_boxed_str()));
        let _ = PROSE.set(family);
    });
}

/// The font for answer prose (libcosmic's until [load] found the desktop's).
pub fn prose() -> cosmic::font::Font {
    match PROSE.get().copied().flatten() {
        Some(name) => cosmic::font::Font { family: cosmic::iced::font::Family::Name(name), ..cosmic::font::default() },
        None => cosmic::font::default(),
    }
}

pub fn prose_weight(weight: cosmic::iced::font::Weight) -> cosmic::font::Font {
    cosmic::font::Font { weight, ..prose() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_names() {
        assert_eq!(from_gsettings("'Noto Sans  10'\n").as_deref(), Some("Noto Sans"));
        assert_eq!(from_gsettings("'Cantarell Bold 11.5'").as_deref(), Some("Cantarell Bold"));
        assert_eq!(from_gsettings("'Inter'").as_deref(), Some("Inter"));
        assert_eq!(from_gsettings("''"), None);
        let kde = "[Colors:View]\nfont=Wrong,9\n[General]\nColorScheme=X\nfont=Noto Sans,10,-1,5,400,0,0,0,0,0\nfixed=Hack,10\n";
        assert_eq!(from_kdeglobals(kde).as_deref(), Some("Noto Sans"));
        assert_eq!(from_kdeglobals("[General]\nfixed=Hack,10\n"), None);
        assert_eq!(desktop_family(Desktop::Hyprland), None, "tests never read the session");
    }
}
