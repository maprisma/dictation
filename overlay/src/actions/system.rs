//! The desktop settings the level-0 actions change, as pure functions (exec.rs runs them):
//! volume (wpctl), do not disturb (Maprisma Notifier's settings), light/dark (Maprisma Theme).

use serde_json::Value;
use std::path::{Path, PathBuf};

pub const SINK: &str = "@DEFAULT_AUDIO_SINK@";

/// `wpctl get-volume` → (volume 0…1.5, muted).
pub fn parse_volume(out: &str) -> Option<(f64, bool)> {
    let rest = out.trim().strip_prefix("Volume:")?.trim();
    let number = rest.split_whitespace().next()?.parse::<f64>().ok()?;
    Some((number, rest.contains("[MUTED]")))
}

fn wpctl(args: &[&str]) -> Vec<String> {
    std::iter::once("wpctl").chain(args.iter().copied()).map(String::from).collect()
}

/// The commands for a volume action, and the state after them. Never above 100 %.
pub fn volume_commands(current: (f64, bool), percent: Option<u32>, mute: Option<bool>, change: Option<i32>) -> (Vec<Vec<String>>, (f64, bool)) {
    let mut cmds = vec![];
    let mut volume = current.0;
    let mut muted = current.1;
    let target = match (percent, change) {
        (Some(p), _) => Some(p.min(100) as f64 / 100.0),
        (None, Some(c)) => Some(((current.0 * 100.0).round() + c as f64).clamp(0.0, 100.0) / 100.0),
        _ => None,
    };
    if let Some(t) = target {
        volume = t;
        cmds.push(wpctl(&["set-volume", SINK, &format!("{t:.2}")]));
        // A volume change on a muted sink is meant to be heard (unless mute was asked for too).
        if mute.is_none() && muted && t > 0.0 {
            muted = false;
            cmds.push(wpctl(&["set-mute", SINK, "0"]));
        }
    }
    if let Some(m) = mute {
        muted = m;
        cmds.push(wpctl(&["set-mute", SINK, if m { "1" } else { "0" }]));
    }
    (cmds, (volume, muted))
}

/// Back to an earlier state.
pub fn volume_restore(state: (f64, bool)) -> Vec<Vec<String>> {
    vec![wpctl(&["set-volume", SINK, &format!("{:.2}", state.0)]), wpctl(&["set-mute", SINK, if state.1 { "1" } else { "0" }])]
}

// ------------------------------------------------------------------- do not disturb (Notifier)

pub fn notifier_settings() -> PathBuf {
    crate::paths::config_root().join("maprisma-notifier/settings.json")
}

pub fn dnd_of(bytes: Option<&[u8]>) -> bool {
    bytes.and_then(|b| serde_json::from_slice::<Value>(b).ok()).and_then(|v| v.get("dnd").and_then(Value::as_bool)).unwrap_or(false)
}

/// Maprisma Notifier's recorder is what applies its `dnd` key to the desktop.
pub fn notifier_installed(path_env: &str) -> bool {
    crate::helper::which_in("maprisma-notifier-recorder", path_env).is_some()
}

/// The Notifier's lock for a settings file it shares with other writers: an open file description
/// lock (`fcntl F_OFD_SETLKW`) on `<file>.lock`, held until dropped (maprisma-notifier
/// `paths::JsonLock`, docs/plan.md "Writing the settings").
pub struct JsonLock(#[allow(dead_code)] std::fs::File);

impl JsonLock {
    pub fn take(path: &Path) -> std::io::Result<Self> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let lock = path.with_file_name(format!("{name}.lock"));
        if let Some(parent) = lock.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600).custom_flags(libc::O_CLOEXEC).open(&lock)?;
        // SAFETY: a zeroed flock is a valid "whole file" range; fcntl only reads it.
        let mut range: libc::flock = unsafe { std::mem::zeroed() };
        range.l_type = libc::F_WRLCK as _;
        range.l_whence = libc::SEEK_SET as _;
        loop {
            // SAFETY: fcntl on a descriptor this File owns, with a valid flock.
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLKW, &range) } == 0 {
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

/// The settings with one key set, every other key kept (a broken file starts empty).
pub fn with_key(bytes: Option<&[u8]>, key: &str, value: Value) -> Vec<u8> {
    let mut object = match bytes.and_then(|b| serde_json::from_slice::<Value>(b).ok()) {
        Some(Value::Object(m)) => m,
        _ => Default::default(),
    };
    object.insert(key.into(), value);
    let mut text = serde_json::to_string_pretty(&Value::Object(object)).unwrap_or_default();
    text.push('\n');
    text.into_bytes()
}

/// tmp + rename, 0600.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no folder"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.{}.tmp", path.file_name().and_then(|f| f.to_str()).unwrap_or("settings"), std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

// ------------------------------------------------------------------- light/dark (Maprisma Theme)

/// Maprisma Theme's themes (user first, then the shipped ones) and its state files.
#[derive(Debug, Clone)]
pub struct ThemeFiles {
    pub theme_dirs: Vec<PathBuf>,
    pub current: PathBuf,
    pub recent: PathBuf,
    pub hidden: PathBuf,
}

pub fn theme_files() -> ThemeFiles {
    let config = crate::paths::config_root().join("maprisma-theme");
    let state = crate::paths::state_root().join("maprisma-theme");
    let mut dirs = vec![config.join("themes")];
    if let Some(share) = std::env::var_os("MAPRISMA_THEME_SHARE") {
        dirs.push(PathBuf::from(share).join("themes"));
    }
    // share/maprisma-theme next to the installed core (~/.local/lib/maprisma-theme/bin/…).
    if let Some(core) = crate::helper::which_in("maprisma-theme-core", &std::env::var("PATH").unwrap_or_default()).and_then(|p| std::fs::canonicalize(p).ok()) {
        if let Some(bin) = core.parent() {
            dirs.push(bin.join("../share/maprisma-theme/themes"));
        }
    }
    dirs.push(PathBuf::from("/usr/share/maprisma-theme/themes"));
    ThemeFiles { theme_dirs: dirs, current: state.join("current.json"), recent: config.join("recent.json"), hidden: config.join("hidden.json") }
}

fn luminance(hex: &str) -> Option<f64> {
    let h = hex.trim().trim_start_matches('#');
    if h.len() < 6 || !h.is_char_boundary(6) {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(|v| {
        let s = v as f64 / 255.0;
        if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    });
    Some(0.2126 * c(0)? + 0.7152 * c(2)? + 0.0722 * c(4)?)
}

/// Light like Maprisma Theme decides it: a `light.mode` file, else the background's luminance.
pub fn theme_is_light(light_mode_file: bool, colors_toml: &str) -> bool {
    if light_mode_file {
        return true;
    }
    for line in colors_toml.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == "background" {
                let v = v.split('#').nth(1).map(|h| format!("#{}", h.trim_matches(|c: char| !c.is_ascii_hexdigit())));
                return v.and_then(|h| luminance(&h)).is_some_and(|l| l > 0.5);
            }
        }
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeEntry {
    pub slug: String,
    pub light: bool,
    pub hidden: bool,
}

pub fn scan_themes(files: &ThemeFiles) -> Vec<ThemeEntry> {
    let hidden: Vec<String> = std::fs::read(&files.hidden).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut out: Vec<ThemeEntry> = vec![];
    for dir in &files.theme_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let slug = e.file_name().to_string_lossy().into_owned();
            if slug.starts_with('.') || !e.path().is_dir() || out.iter().any(|t| t.slug == slug) || !crate::helper::plain(&slug) {
                continue;
            }
            let colors = std::fs::read_to_string(e.path().join("colors.toml")).unwrap_or_default();
            if colors.is_empty() {
                continue;
            }
            out.push(ThemeEntry { light: theme_is_light(e.path().join("light.mode").exists(), &colors), hidden: hidden.contains(&slug), slug });
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

pub fn current_theme(bytes: Option<&[u8]>) -> Option<String> {
    let v: Value = serde_json::from_slice(bytes?).ok()?;
    v["theme"].as_str().filter(|s| crate::helper::plain(s)).map(String::from)
}

/// The theme to switch to for a mode: none when the current one already is that mode; else the
/// most recently used one of that mode, preferring themes not hidden in Maprisma Theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemePick {
    Already(String),
    Switch(String),
    NoneOfThatMode,
}

pub fn pick_theme(themes: &[ThemeEntry], current: Option<&str>, recent: &serde_json::Map<String, Value>, light: bool) -> ThemePick {
    if let Some(cur) = current.and_then(|c| themes.iter().find(|t| t.slug == c)) {
        if cur.light == light {
            return ThemePick::Already(cur.slug.clone());
        }
    }
    let mut candidates: Vec<&ThemeEntry> = themes.iter().filter(|t| t.light == light).collect();
    if candidates.iter().any(|t| !t.hidden) {
        candidates.retain(|t| !t.hidden);
    }
    let used = |t: &ThemeEntry| recent.get(&t.slug).and_then(Value::as_f64).unwrap_or(0.0);
    candidates.sort_by(|a, b| used(b).partial_cmp(&used(a)).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.slug.cmp(&b.slug)));
    match candidates.first() {
        Some(t) => ThemePick::Switch(t.slug.clone()),
        None => ThemePick::NoneOfThatMode,
    }
}

pub fn theme_apply_argv(slug: &str) -> Vec<String> {
    vec!["maprisma-theme-core".into(), "apply".into(), slug.into()]
}

/// `maprisma-theme-core apply` prints JSON; `{"error": …}` is a failure.
pub fn theme_apply_error(stdout: &str) -> Option<String> {
    let v: Value = serde_json::from_str(stdout.trim().lines().last().unwrap_or("")).ok()?;
    let e = v.get("error")?.as_str()?.to_string();
    Some(v.get("message").and_then(Value::as_str).map(|m| format!("{e}: {m}")).unwrap_or(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dnd_needs_the_notifier_and_takes_its_lock() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("maprisma-dictation-dnd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!notifier_installed(dir.to_str().unwrap()));
        let bin = dir.join("maprisma-notifier-recorder");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(notifier_installed(&format!("/nonexistent:{}", dir.display())));
        let settings = dir.join("notifier/settings.json");
        let lock = JsonLock::take(&settings).unwrap();
        assert!(dir.join("notifier/settings.json.lock").is_file());
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn volume() {
        assert_eq!(parse_volume("Volume: 0.40\n"), Some((0.40, false)));
        assert_eq!(parse_volume("Volume: 0.55 [MUTED]"), Some((0.55, true)));
        assert_eq!(parse_volume("nope"), None);
        let (cmds, after) = volume_commands((0.40, false), None, None, Some(15));
        assert_eq!(cmds, vec![wpctl(&["set-volume", SINK, "0.55"])]);
        assert_eq!(after, (0.55, false));
        let (cmds, after) = volume_commands((0.95, true), None, None, Some(20));
        assert_eq!(cmds, vec![wpctl(&["set-volume", SINK, "1.00"]), wpctl(&["set-mute", SINK, "0"])]);
        assert_eq!(after, (1.0, false));
        let (cmds, after) = volume_commands((0.30, false), Some(70), Some(true), None);
        assert_eq!(cmds.len(), 2);
        assert_eq!(after, (0.70, true));
        let (cmds, after) = volume_commands((0.05, false), None, None, Some(-20));
        assert_eq!(cmds, vec![wpctl(&["set-volume", SINK, "0.00"])]);
        assert_eq!(after.0, 0.0);
        assert_eq!(volume_restore((0.4, true))[1], wpctl(&["set-mute", SINK, "1"]));
    }

    #[test]
    fn dnd_keeps_other_keys() {
        let old = br#"{"dnd": false, "window_size": {"width": 420}, "muted_apps": ["x"]}"#;
        assert!(!dnd_of(Some(old)));
        let new = with_key(Some(old), "dnd", true.into());
        let v: Value = serde_json::from_slice(&new).unwrap();
        assert_eq!(v["dnd"], true);
        assert_eq!(v["window_size"]["width"], 420);
        assert_eq!(v["muted_apps"][0], "x");
        assert!(dnd_of(Some(&new)));
        assert!(!dnd_of(None) && !dnd_of(Some(b"[broken")));
        assert_eq!(serde_json::from_slice::<Value>(&with_key(Some(b"[1]"), "dnd", true.into())).unwrap(), json!({"dnd": true}));
        let path = crate::paths::test_roots().join("config/maprisma-notifier/settings.json");
        write_atomic(&path, &new).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(dnd_of(std::fs::read(&path).ok().as_deref()));
    }

    #[test]
    fn theme_mode() {
        assert!(theme_is_light(false, "accent = \"#000\"\nbackground = \"#eff1f5\"\n"));
        assert!(!theme_is_light(false, "background           = \"#1a2234\"   # comment\n"));
        assert!(theme_is_light(true, "background = \"#000000\""));
        assert!(!theme_is_light(false, "foreground = \"#ffffff\""));
        let t = |slug: &str, light: bool, hidden: bool| ThemeEntry { slug: slug.into(), light, hidden };
        let themes = vec![t("autumn", false, false), t("nord", false, false), t("latte", true, true), t("white", true, false), t("paper", true, false)];
        let recent = json!({"paper": 10.0, "white": 20.0, "latte": 99.0, "nord": 5.0}).as_object().unwrap().clone();
        assert_eq!(pick_theme(&themes, Some("autumn"), &recent, true), ThemePick::Switch("white".into()));
        assert_eq!(pick_theme(&themes, Some("autumn"), &recent, false), ThemePick::Already("autumn".into()));
        assert_eq!(pick_theme(&themes, Some("white"), &recent, false), ThemePick::Switch("nord".into()));
        assert_eq!(pick_theme(&themes[..2], Some("autumn"), &recent, true), ThemePick::NoneOfThatMode);
        // only hidden ones: still better than nothing
        assert_eq!(pick_theme(&themes[..3], None, &recent, true), ThemePick::Switch("latte".into()));
        assert_eq!(current_theme(Some(br#"{"theme": "autumn", "wallpaper": "/x"}"#)).as_deref(), Some("autumn"));
        assert_eq!(current_theme(Some(br#"{"theme": "../x"}"#)), None);
        assert_eq!(theme_apply_error(r#"{"error": "not_found", "message": "no theme"}"#).as_deref(), Some("not_found: no theme"));
        assert_eq!(theme_apply_error(r#"{"applied": true}"#), None);
        assert_eq!(theme_apply_argv("nord"), ["maprisma-theme-core", "apply", "nord"]);
    }
}
