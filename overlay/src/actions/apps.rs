//! `open_app`: finding an installed app by what the user called it. The desktop entries come
//! from `$XDG_DATA_HOME/applications` and `$XDG_DATA_DIRS/*/applications` (plus the Flatpak
//! export folders), the first entry of an id winning like the desktop does. A name matches the
//! entry's Name (also the localized one), GenericName, Keywords or its id, exactly, as a prefix or
//! word, or with one typo; several equally good matches are offered as choices.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    /// The desktop id (`org.mozilla.firefox.desktop`).
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub generic: String,
    pub keywords: Vec<String>,
    pub icon: String,
    /// Found in a folder `gtk-launch` searches (else `gio launch <path>`).
    pub standard: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    One(App),
    /// Several fit as well: the user picks (at most 5).
    Choices(Vec<App>),
    None,
}

/// The folders, in the desktop's order.
pub fn data_dirs(data_home: &Path, data_dirs: &str) -> Vec<(PathBuf, bool)> {
    let mut dirs = vec![(data_home.join("applications"), true)];
    let list = if data_dirs.trim().is_empty() { "/usr/local/share:/usr/share" } else { data_dirs };
    for d in list.split(':').filter(|d| d.starts_with('/')) {
        dirs.push((Path::new(d).join("applications"), true));
    }
    for extra in [data_home.join("flatpak/exports/share/applications"), PathBuf::from("/var/lib/flatpak/exports/share/applications")] {
        if !dirs.iter().any(|(d, _)| *d == extra) {
            dirs.push((extra, false));
        }
    }
    dirs
}

/// The value of `key` (or `key[lang]`, preferred) in the [Desktop Entry] group.
fn entry_value<'a>(text: &'a str, key: &str, lang: &str) -> Option<&'a str> {
    let mut group = "";
    let (mut plain, mut local) = (None, None);
    let localized = format!("{key}[{lang}]");
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            group = line;
            continue;
        }
        if group != "[Desktop Entry]" {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            if k == key {
                plain = Some(v.trim());
            } else if !lang.is_empty() && k == localized {
                local = Some(v.trim());
            }
        }
    }
    local.or(plain)
}

pub fn parse_entry(id: &str, path: &Path, text: &str, lang: &str, desktop: &str, standard: bool) -> Option<App> {
    if entry_value(text, "Type", "")? != "Application" {
        return None;
    }
    let yes = |k: &str| entry_value(text, k, "").is_some_and(|v| v.eq_ignore_ascii_case("true"));
    if yes("NoDisplay") || yes("Hidden") || entry_value(text, "Exec", "").is_none_or(str::is_empty) {
        return None;
    }
    let desktops = |k: &str| entry_value(text, k, "").map(|v| v.split(';').filter(|s| !s.is_empty()).map(|s| s.to_ascii_lowercase()).collect::<Vec<_>>());
    let current: Vec<String> = desktop.split(':').map(|s| s.to_ascii_lowercase()).filter(|s| !s.is_empty()).collect();
    if let Some(only) = desktops("OnlyShowIn") {
        if !only.iter().any(|d| current.contains(d)) {
            return None;
        }
    }
    if let Some(not) = desktops("NotShowIn") {
        if not.iter().any(|d| current.contains(d)) {
            return None;
        }
    }
    let name = entry_value(text, "Name", lang)?.to_string();
    let mut keywords: Vec<String> = entry_value(text, "Keywords", lang).unwrap_or("").split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    // The English ones too: people say "browser" in any language.
    if !lang.is_empty() {
        for k in entry_value(text, "Keywords", "").unwrap_or("").split(';').map(str::trim).filter(|s| !s.is_empty()) {
            if !keywords.iter().any(|x| x == k) {
                keywords.push(k.to_string());
            }
        }
    }
    let mut generic = entry_value(text, "GenericName", lang).unwrap_or("").to_string();
    if let Some(english) = entry_value(text, "GenericName", "").filter(|g| !lang.is_empty() && *g != generic) {
        generic = format!("{generic};{english}");
    }
    Some(App { id: id.to_string(), path: path.to_path_buf(), name, generic, keywords, icon: entry_value(text, "Icon", "").unwrap_or("").to_string(), standard })
}

/// Every app the user can see (one per id; the first folder wins).
pub fn scan(dirs: &[(PathBuf, bool)], lang: &str, desktop: &str) -> Vec<App> {
    let mut seen = HashSet::new();
    let mut apps = vec![];
    for (dir, standard) in dirs {
        let mut stack = vec![(dir.clone(), String::new())];
        while let Some((d, prefix)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let file = e.file_name().to_string_lossy().into_owned();
                let path = e.path();
                if path.is_dir() {
                    if prefix.matches('-').count() < 4 {
                        stack.push((path, format!("{prefix}{file}-")));
                    }
                    continue;
                }
                if !file.ends_with(".desktop") {
                    continue;
                }
                let id = format!("{prefix}{file}");
                if !seen.insert(id.clone()) {
                    continue;
                }
                let Ok(meta) = std::fs::metadata(&path) else { continue };
                if meta.len() > 256 * 1024 {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Some(app) = parse_entry(&id, &path, &text, lang, desktop, *standard) {
                        apps.push(app);
                    }
                }
            }
        }
    }
    apps
}

/// Lower case letters and digits only.
pub fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect()
}

/// One typo (insert, delete, change or swap of two letters).
fn one_typo(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a == b || a.len().abs_diff(b.len()) > 1 {
        return a == b;
    }
    if a.len() == b.len() {
        let diff: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
        return diff.len() == 1 || (diff.len() == 2 && diff[1] == diff[0] + 1 && a[diff[0]] == b[diff[1]] && a[diff[1]] == b[diff[0]]);
    }
    let (long, short) = if a.len() > b.len() { (&a, &b) } else { (&b, &a) };
    let mut i = 0;
    while i < short.len() && long[i] == short[i] {
        i += 1;
    }
    long[i + 1..] == short[i..]
}

/// How well an app fits what was asked (0: not at all).
pub fn score(app: &App, query: &str) -> u32 {
    let q = norm(query);
    if q.is_empty() {
        return 0;
    }
    let name = norm(&app.name);
    let id_stem = app.id.trim_end_matches(".desktop");
    let id_last = norm(id_stem.rsplit(['.', '-']).next().unwrap_or(id_stem));
    let id_all = norm(id_stem);
    let mut best = 0;
    let mut take = |s: u32| best = best.max(s);
    if name == q {
        take(100);
    }
    if id_last == q || id_all == q {
        take(90);
    }
    if name.starts_with(&q) && q.len() >= 3 {
        take(80);
    }
    if words(&app.name).iter().any(|w| norm(w) == q) {
        take(75);
    }
    // "calculator app", "the Firefox browser": the name inside what was said.
    if name.len() >= 4 && (q.starts_with(&name) || words(query).iter().any(|w| norm(w) == name)) {
        take(70);
    }
    if q.len() >= 4 && name.contains(&q) {
        take(60);
    }
    for g in app.generic.split(';').filter(|g| !g.is_empty()) {
        let g = norm(g);
        if g == q {
            take(50);
        } else if q.len() >= 4 && (g.starts_with(&q) || words(&app.generic).iter().any(|w| norm(w) == q)) {
            take(35);
        }
    }
    for k in &app.keywords {
        let k = norm(k);
        if k == q {
            take(45);
        } else if q.len() >= 4 && k.starts_with(&q) {
            take(30);
        }
    }
    if q.len() >= 5 && (one_typo(&name, &q) || one_typo(&id_last, &q)) {
        take(40);
    }
    best
}

/// The app for a name: one clear best, or the best few to choose from.
pub fn resolve(apps: &[App], query: &str) -> Found {
    let mut scored: Vec<(u32, &App)> = apps.iter().map(|a| (score(a, query), a)).filter(|(s, _)| *s >= 30).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase())));
    let Some(&(top, first)) = scored.first() else { return Found::None };
    let ties: Vec<&App> = scored.iter().filter(|(s, _)| *s == top).map(|(_, a)| *a).collect();
    if ties.len() == 1 && (top >= 75 || scored.len() == 1 || scored[1].0 + 20 <= top) {
        return Found::One(first.clone());
    }
    // Several fit about as well: offer the best ones.
    let near: Vec<App> = scored.iter().filter(|(s, _)| *s + 20 > top).take(5).map(|(_, a)| (*a).clone()).collect();
    if near.len() == 1 { Found::One(near[0].clone()) } else { Found::Choices(near) }
}

/// The command that starts it: `gtk-launch <id>` for the standard folders, else `gio launch <path>`.
pub fn launch_argv(app: &App) -> Vec<String> {
    if app.standard {
        vec!["gtk-launch".into(), app.id.trim_end_matches(".desktop").into()]
    } else {
        vec!["gio".into(), "launch".into(), app.path.to_string_lossy().into_owned()]
    }
}

/// The session's folders and language.
pub fn installed(lang: &str) -> Vec<App> {
    let home = crate::paths::home();
    let data_home = match std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => home.join(".local/share"),
    };
    let dirs = data_dirs(&data_home, &std::env::var("XDG_DATA_DIRS").unwrap_or_default());
    scan(&dirs, lang, &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, file: &str, text: &str) {
        std::fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
        std::fs::write(dir.join(file), text).unwrap();
    }

    fn entry(name: &str, extra: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec=x\n{extra}\n[Desktop Action new]\nName=Other\n")
    }

    /// Each test its own folders (tests run in parallel).
    fn fake(name: &str) -> (PathBuf, Vec<(PathBuf, bool)>) {
        let root = crate::paths::test_roots().join(format!("apps-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        let user = root.join("home/applications");
        let sys = root.join("usr/applications");
        let flat = root.join("flatpak/applications");
        write(&sys, "firefox.desktop", &entry("Firefox", "GenericName=Web Browser\nGenericName[de]=Webbrowser\nKeywords=Internet;WWW;Browser;\nName[de]=Firefox Browser"));
        write(&sys, "google-chrome.desktop", &entry("Google Chrome", "GenericName=Web Browser\nKeywords=browser;web"));
        write(&sys, "org.gnome.Calculator.desktop", &entry("Calculator", "Keywords=calc;math;"));
        write(&sys, "kitty.desktop", &entry("kitty", "GenericName=Terminal emulator"));
        write(&sys, "hidden.desktop", &entry("Hidden thing", "NoDisplay=true"));
        write(&sys, "kde-only.desktop", &entry("Konsole", "OnlyShowIn=KDE;"));
        write(&sys, "link.desktop", "[Desktop Entry]\nType=Link\nName=Link\nURL=x\n");
        write(&sys, "kde/okular.desktop", &entry("Okular", ""));
        // the user's copy hides the system one
        write(&user, "kitty.desktop", &entry("Kitty (mine)", ""));
        write(&flat, "org.signal.Signal.desktop", &entry("Signal", "Keywords=chat;"));
        let dirs = vec![(user, true), (sys, true), (flat, false)];
        (root, dirs)
    }

    #[test]
    fn scanning_folders() {
        let dirs = data_dirs(Path::new("/h/.local/share"), "");
        assert_eq!(dirs[0].0, PathBuf::from("/h/.local/share/applications"));
        assert!(dirs.iter().any(|(d, s)| d == Path::new("/usr/share/applications") && *s));
        assert!(dirs.iter().any(|(d, s)| d.ends_with("flatpak/exports/share/applications") && !*s));
        let (_root, dirs) = fake("scan");
        let apps = scan(&dirs, "de", "Hyprland");
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"Firefox Browser"), "{names:?}");
        assert!(names.contains(&"Kitty (mine)") && !names.contains(&"kitty"));
        assert!(!names.contains(&"Hidden thing") && !names.contains(&"Konsole") && !names.contains(&"Link"));
        assert!(apps.iter().any(|a| a.id == "kde-okular.desktop"));
        assert_eq!(scan(&dirs, "", "KDE").iter().filter(|a| a.name == "Konsole").count(), 1);
    }

    #[test]
    fn resolving_names() {
        let (_root, dirs) = fake("resolve");
        let apps = scan(&dirs, "", "Hyprland");
        let one = |q: &str| match resolve(&apps, q) {
            Found::One(a) => a.name,
            other => panic!("{q}: {other:?}"),
        };
        assert_eq!(one("Firefox"), "Firefox");
        assert_eq!(one("firefox"), "Firefox");
        assert_eq!(one("firefx"), "Firefox"); // one typo
        assert_eq!(one("chrome"), "Google Chrome");
        assert_eq!(one("calculator"), "Calculator");
        assert_eq!(one("calc"), "Calculator"); // keyword
        assert_eq!(one("signal"), "Signal");
        assert_eq!(one("Calculator app"), "Calculator");
        assert_eq!(one("the firefox browser"), "Firefox");
        match resolve(&apps, "web browser") {
            Found::Choices(c) => {
                let names: Vec<&str> = c.iter().map(|a| a.name.as_str()).collect();
                assert_eq!(names, ["Firefox", "Google Chrome"]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(resolve(&apps, "photoshop"), Found::None);
        assert_eq!(resolve(&apps, ""), Found::None);
        let signal = apps.iter().find(|a| a.name == "Signal").unwrap();
        assert_eq!(launch_argv(signal)[..2], ["gio".to_string(), "launch".to_string()]);
        let firefox = apps.iter().find(|a| a.name == "Firefox").unwrap();
        assert_eq!(launch_argv(firefox), ["gtk-launch", "firefox"]);
    }
}
