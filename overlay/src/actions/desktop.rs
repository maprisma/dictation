//! Workspaces and windows per desktop: the commands are built here (tested without running
//! anything) and run by exec.rs.
//!
//! | | workspace N | move window to N | close window |
//! |---|---|---|---|
//! | Hyprland | `hyprctl dispatch hl.dsp.focus({workspace = "N"})`, else `workspace N` | `hl.dsp.window.move({workspace = "N", window = "address:…"})`, else `movetoworkspace N,address:…` | `hl.dsp.window.close({window = "address:…"})`, else `closewindow address:…` |
//! | KDE Plasma | `org.kde.KWin /KWin setCurrentDesktop(N)` (must answer `true`) | a KWin script | a KWin script |
//!
//! KWin runs a script without telling whether it did anything, so each script reports
//! `ok`, `no-window`, `no-desktop` or `error: …` back to the overlay's `KwinResult`
//! ([kwin_script]); no report means it failed.
//! | COSMIC | no interface for other apps: unsupported | unsupported | unsupported |

use crate::look::Desktop;

/// One way of doing it; exec.rs tries them in order until one works.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `hyprctl` with these arguments; it worked when it prints `ok`.
    Hyprctl(Vec<String>),
    /// `org.kde.KWin /KWin org.kde.KWin.setCurrentDesktop(i)`.
    KwinDesktop(i32),
    /// A KWin script (JavaScript) loaded and run through org.kde.kwin.Scripting; its body calls
    /// `report(…)` ([kwin_script] adds that function).
    KwinScript(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Try(Vec<Step>),
    Unsupported,
}

/// A Hyprland window address as hyprctl writes it (0x and hex digits).
pub fn hypr_address(a: &str) -> bool {
    a.len() > 2 && a.len() <= 20 && a.starts_with("0x") && a[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn hyprctl(args: &[&str]) -> Step {
    Step::Hyprctl(args.iter().map(|s| s.to_string()).collect())
}

pub fn workspace(desktop: Desktop, n: u32) -> Plan {
    match desktop {
        Desktop::Hyprland => Plan::Try(vec![hyprctl(&["dispatch", &format!("hl.dsp.focus({{workspace = \"{n}\"}})")]), hyprctl(&["dispatch", "workspace", &n.to_string()])]),
        Desktop::Kde => Plan::Try(vec![Step::KwinDesktop(n as i32)]),
        Desktop::Cosmic | Desktop::Other => Plan::Unsupported,
    }
}

/// The most recently used normal window on the current desktop (not the overlay itself, which
/// KWin may count as active while it has the keyboard).
const KWIN_TARGET: &str = "const list = workspace.stackingOrder.filter(w => w.normalWindow && !w.minimized && !w.deleted && (w.onAllDesktops || w.desktops.indexOf(workspace.currentDesktop) >= 0)); \
const a = workspace.activeWindow; const w = (a && a.normalWindow) ? a : list[list.length - 1];";

/// `window`: the Hyprland address of the window the action is for.
pub fn move_window(desktop: Desktop, n: u32, window: Option<&str>) -> Plan {
    match desktop {
        Desktop::Hyprland => match window.filter(|w| hypr_address(w)) {
            Some(w) => Plan::Try(vec![
                hyprctl(&["dispatch", &format!("hl.dsp.window.move({{workspace = \"{n}\", window = \"address:{w}\"}})")]),
                hyprctl(&["dispatch", "movetoworkspace", &format!("{n},address:{w}")]),
            ]),
            None => Plan::Unsupported,
        },
        Desktop::Kde => Plan::Try(vec![Step::KwinScript(format!(
            "(function () {{ {KWIN_TARGET} const d = workspace.desktops[{}]; if (!w) report(\"no-window\"); else if (!d) report(\"no-desktop\"); else {{ w.desktops = [d]; workspace.currentDesktop = d; workspace.activeWindow = w; report(\"ok\"); }} }})();",
            n - 1
        ))]),
        Desktop::Cosmic | Desktop::Other => Plan::Unsupported,
    }
}

pub fn close_window(desktop: Desktop, window: Option<&str>) -> Plan {
    match desktop {
        Desktop::Hyprland => match window.filter(|w| hypr_address(w)) {
            Some(w) => Plan::Try(vec![hyprctl(&["dispatch", &format!("hl.dsp.window.close({{window = \"address:{w}\"}})")]), hyprctl(&["dispatch", "closewindow", &format!("address:{w}")])]),
            None => Plan::Unsupported,
        },
        Desktop::Kde => Plan::Try(vec![Step::KwinScript(format!("(function () {{ {KWIN_TARGET} if (!w) report(\"no-window\"); else {{ w.closeWindow(); report(\"ok\"); }} }})();"))]),
        Desktop::Cosmic | Desktop::Other => Plan::Unsupported,
    }
}

/// The script KWin loads: `report(r)` calls the overlay's `KwinResult(token, r)`, and an
/// exception is reported as `error: …`. `token` is only letters, digits, `_` and `-`.
pub fn kwin_script(body: &str, token: &str) -> String {
    let token: String = token.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').take(64).collect();
    format!(
        "function report(r) {{ callDBus(\"{}\", \"{}\", \"{}\", \"KwinResult\", \"{token}\", String(r)); }}\ntry {{ {body} }} catch (e) {{ report(\"error: \" + e); }}\n",
        crate::instance::NAME,
        crate::instance::PATH,
        crate::instance::INTERFACE,
    )
}

/// `hyprctl -j activeworkspace` → its id.
pub fn hypr_workspace_id(json: &[u8]) -> Option<u32> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    v["id"].as_i64().filter(|i| *i >= 1).map(|i| i as u32)
}

/// The active window, from `hyprctl -j activewindow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyprWindow {
    pub address: String,
    pub title: String,
    pub class: String,
    pub workspace: u32,
    pub pid: i64,
}

pub fn hypr_active_window(json: &[u8]) -> Option<HyprWindow> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let address = v["address"].as_str()?.to_string();
    if !hypr_address(&address) {
        return None;
    }
    Some(HyprWindow {
        address,
        title: v["title"].as_str().unwrap_or("").chars().take(120).collect(),
        class: v["class"].as_str().unwrap_or("").chars().take(80).collect(),
        workspace: v["workspace"]["id"].as_i64().filter(|i| *i >= 1).unwrap_or(0) as u32,
        pid: v["pid"].as_i64().unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyprland_commands() {
        assert_eq!(
            workspace(Desktop::Hyprland, 3),
            Plan::Try(vec![Step::Hyprctl(vec!["dispatch".into(), "hl.dsp.focus({workspace = \"3\"})".into()]), Step::Hyprctl(vec!["dispatch".into(), "workspace".into(), "3".into()])])
        );
        let Plan::Try(steps) = move_window(Desktop::Hyprland, 2, Some("0x55d1a2")) else { panic!() };
        assert_eq!(steps[0], Step::Hyprctl(vec!["dispatch".into(), "hl.dsp.window.move({workspace = \"2\", window = \"address:0x55d1a2\"})".into()]));
        assert_eq!(steps[1], Step::Hyprctl(vec!["dispatch".into(), "movetoworkspace".into(), "2,address:0x55d1a2".into()]));
        let Plan::Try(steps) = close_window(Desktop::Hyprland, Some("0xabc")) else { panic!() };
        assert_eq!(steps[1], Step::Hyprctl(vec!["dispatch".into(), "closewindow".into(), "address:0xabc".into()]));
        // no (or a strange) address: nothing to aim at
        assert_eq!(close_window(Desktop::Hyprland, None), Plan::Unsupported);
        assert_eq!(move_window(Desktop::Hyprland, 2, Some("0x1\"})")), Plan::Unsupported);
    }

    #[test]
    fn kde_and_cosmic_commands() {
        assert_eq!(workspace(Desktop::Kde, 4), Plan::Try(vec![Step::KwinDesktop(4)]));
        let Plan::Try(steps) = move_window(Desktop::Kde, 2, None) else { panic!() };
        let Step::KwinScript(js) = &steps[0] else { panic!() };
        assert!(js.contains("workspace.desktops[1]") && js.contains("w.desktops = [d]"));
        // every way out reports: a missing window or desktop is no success
        assert!(js.contains("report(\"no-window\")") && js.contains("report(\"no-desktop\")") && js.contains("report(\"ok\")"));
        let full = kwin_script(js, "maprisma_1_2\"); evil");
        assert!(full.contains("callDBus(\"com.maprisma.Dictation\", \"/com/maprisma/Dictation\", \"com.maprisma.Dictation\", \"KwinResult\", \"maprisma_1_2evil\""));
        assert!(full.contains("catch (e) { report(\"error: \" + e); }"));
        let Plan::Try(steps) = close_window(Desktop::Kde, None) else { panic!() };
        assert!(matches!(&steps[0], Step::KwinScript(js) if js.contains("closeWindow()") && js.contains("normalWindow") && js.contains("report(\"no-window\")")));
        for plan in [workspace(Desktop::Cosmic, 1), move_window(Desktop::Cosmic, 1, None), close_window(Desktop::Cosmic, None), workspace(Desktop::Other, 1)] {
            assert_eq!(plan, Plan::Unsupported);
        }
    }

    #[test]
    fn hyprctl_json() {
        assert_eq!(hypr_workspace_id(br#"{"id": 3, "name": "3"}"#), Some(3));
        assert_eq!(hypr_workspace_id(br#"{"id": -98, "name": "special"}"#), None);
        let w = hypr_active_window(br#"{"address": "0x5f2a", "title": "Mozilla Firefox", "class": "firefox", "workspace": {"id": 2}, "pid": 42}"#).unwrap();
        assert_eq!(w, HyprWindow { address: "0x5f2a".into(), title: "Mozilla Firefox".into(), class: "firefox".into(), workspace: 2, pid: 42 });
        assert_eq!(hypr_active_window(b"{}"), None);
        assert!(!hypr_address("0x") && !hypr_address("5f2a") && hypr_address("0xDEADbeef"));
    }
}
