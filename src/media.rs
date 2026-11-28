//! Optional media ducking / pausing while a dictation session listens, so playing streams
//! (YouTube videos, music, podcasts) don't bleed into the microphone across every Maprisma app.
//!
//! - `Keep`: leave playback untouched (the default).
//! - `Duck`: lower the active output sink's volume to 15 % via `wpctl` (or `pactl`), pinned to
//!   the sink's resolved ID/name so a default-sink switch mid-session cannot strand volume, and
//!   restored when listening ends — unless the user manually adjusted volume meanwhile.
//! - `Pause`: pause all currently-playing MPRIS players over the session D-Bus in parallel
//!   (`org.mpris.MediaPlayer2.*`), and resume only those that are still `Paused` when listening ends.

use crate::paths::Dirs;
use futures_util::future::join_all;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

/// Duck target volume (15 %): quiet enough that speech stands out cleanly, never silent.
pub const DUCK_VOLUME: f64 = 0.15;
/// Tolerance when checking whether the user manually changed volume while ducked.
const VOLUME_TOLERANCE: f64 = 0.03;
/// Timeout for each `wpctl` / `pactl` invocation so a stalled audio daemon never hangs dictation.
const TOOL_TIMEOUT: Duration = Duration::from_secs(1);
/// Per-player D-Bus timeout so a stalled media player never delays dictation.
const MPRIS_TIMEOUT: Duration = Duration::from_millis(350);
/// Overall cap on pausing/resuming all MPRIS players in parallel.
const MPRIS_BUDGET: Duration = Duration::from_millis(700);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaAction {
    #[default]
    Keep,
    Duck,
    Pause,
}

impl MediaAction {
    pub fn parse(s: &str) -> Self {
        match s {
            "duck" => MediaAction::Duck,
            "pause" => MediaAction::Pause,
            _ => MediaAction::Keep,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MediaAction::Keep => "keep",
            MediaAction::Duck => "duck",
            MediaAction::Pause => "pause",
        }
    }
}

#[derive(Debug, Clone)]
enum RestoreState {
    Wpctl { program: PathBuf, target: String, saved_volume: f64 },
    Pactl { program: PathBuf, target: String, saved_percent: u32 },
    Mpris { players: Vec<String> },
}

/// Active media suppression for one listening session. Call [MediaGuard::restore] when listening
/// ends; dropping it without calling `restore` spawns a best-effort cleanup so playback is never
/// left ducked or paused if a task is aborted.
pub struct MediaGuard {
    state: Option<RestoreState>,
}

impl MediaGuard {
    pub fn noop() -> Self {
        MediaGuard { state: None }
    }

    /// Applies `action` right before listening starts. Any failure or timeout is ignored so
    /// dictation never fails because of a missing audio tool or unresponsive player.
    pub async fn acquire(action: MediaAction, dirs: &Dirs) -> Self {
        match action {
            MediaAction::Keep => MediaGuard::noop(),
            MediaAction::Duck => MediaGuard { state: duck_async(dirs).await },
            MediaAction::Pause => {
                let players = pause_mpris().await;
                let state = (!players.is_empty()).then_some(RestoreState::Mpris { players });
                MediaGuard { state }
            }
        }
    }

    /// Restores volume or resumes paused players immediately (idempotent).
    pub async fn restore(&mut self) {
        let Some(state) = self.state.take() else { return };
        restore_state(state).await;
    }
}

impl Drop for MediaGuard {
    fn drop(&mut self) {
        let Some(state) = self.state.take() else { return };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(restore_state(state));
        }
    }
}

async fn restore_state(state: RestoreState) {
    match state {
        RestoreState::Wpctl { program, target, saved_volume } => restore_wpctl(&program, &target, saved_volume).await,
        RestoreState::Pactl { program, target, saved_percent } => restore_pactl(&program, &target, saved_percent).await,
        RestoreState::Mpris { players } => resume_mpris(&players).await,
    }
}

// ------------------------------------------------------------------------- volume ducking

async fn run_capture(program: &PathBuf, args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(TOOL_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn run_quiet(program: &PathBuf, args: &[&str]) -> bool {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(TOOL_TIMEOUT, cmd.status()).await.ok().and_then(Result::ok).is_some_and(|s| s.success())
}

/// Parses `wpctl get-volume <target>`, e.g. `Volume: 0.85` or `Volume: 1.00 [MUTED]`.
/// Returns `(volume, muted)`.
pub fn parse_wpctl_volume(output: &str) -> Option<(f64, bool)> {
    let muted = output.contains("[MUTED]");
    let rest = output.split("Volume:").nth(1)?;
    let num = rest.split_whitespace().next()?.replace(',', ".");
    let vol: f64 = num.parse().ok()?;
    vol.is_finite().then_some((vol.max(0.0), muted))
}

/// Parses the numeric node id from the first line of `wpctl inspect @DEFAULT_AUDIO_SINK@`,
/// e.g. `id 64, type PipeWire:Interface:Node`.
pub fn parse_wpctl_id(output: &str) -> Option<String> {
    let first = output.lines().next()?.trim();
    let rest = first.strip_prefix("id ")?;
    let id = rest.split([',', ' ', '\t']).next()?;
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.to_string())
}

/// Parses the first percentage from `pactl get-sink-volume <target>`, e.g. `... / 85% / ...`.
pub fn parse_pactl_percent(output: &str) -> Option<u32> {
    for token in output.split_whitespace() {
        if let Some(num) = token.strip_suffix('%') {
            if let Ok(p) = num.parse::<u32>() {
                return Some(p);
            }
        }
    }
    None
}

async fn duck_async(dirs: &Dirs) -> Option<RestoreState> {
    if let Some(wpctl) = dirs.which("wpctl") {
        // Resolve the concrete sink node id so restoring hits the exact sink we ducked even if
        // the default sink changes mid-session (e.g. Bluetooth HFP switch).
        let target = run_capture(&wpctl, &["inspect", "@DEFAULT_AUDIO_SINK@"])
            .await
            .and_then(|out| parse_wpctl_id(&out))
            .unwrap_or_else(|| "@DEFAULT_AUDIO_SINK@".to_string());
        let out = run_capture(&wpctl, &["get-volume", &target]).await?;
        let (vol, muted) = parse_wpctl_volume(&out)?;
        if muted || vol <= DUCK_VOLUME + 0.005 {
            return None;
        }
        let duck_str = format!("{DUCK_VOLUME:.2}");
        if !run_quiet(&wpctl, &["set-volume", &target, &duck_str]).await {
            return None;
        }
        return Some(RestoreState::Wpctl { program: wpctl, target, saved_volume: vol });
    }
    if let Some(pactl) = dirs.which("pactl") {
        let target = run_capture(&pactl, &["get-default-sink"])
            .await
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !s.starts_with('-') && !s.chars().any(char::is_control))
            .unwrap_or_else(|| "@DEFAULT_SINK@".to_string());
        let mute_out = run_capture(&pactl, &["get-sink-mute", &target]).await?;
        if mute_out.to_ascii_lowercase().contains("yes") {
            return None;
        }
        let vol_out = run_capture(&pactl, &["get-sink-volume", &target]).await?;
        let pct = parse_pactl_percent(&vol_out)?;
        let duck_pct = (DUCK_VOLUME * 100.0).round() as u32;
        if pct <= duck_pct {
            return None;
        }
        let duck_str = format!("{duck_pct}%");
        if !run_quiet(&pactl, &["set-sink-volume", &target, &duck_str]).await {
            return None;
        }
        return Some(RestoreState::Pactl { program: pactl, target, saved_percent: pct });
    }
    None
}

async fn restore_wpctl(program: &PathBuf, target: &str, saved_volume: f64) {
    // Only restore if volume is still at the ducked level (never overwrite manual user changes).
    let Some(out) = run_capture(program, &["get-volume", target]).await else { return };
    let Some((cur, muted)) = parse_wpctl_volume(&out) else { return };
    if muted || (cur - DUCK_VOLUME).abs() > VOLUME_TOLERANCE {
        return;
    }
    let val = format!("{saved_volume:.2}");
    let _ = run_quiet(program, &["set-volume", target, &val]).await;
}

async fn restore_pactl(program: &PathBuf, target: &str, saved_percent: u32) {
    let duck_pct = (DUCK_VOLUME * 100.0).round() as i32;
    let Some(out) = run_capture(program, &["get-sink-volume", target]).await else { return };
    let Some(cur) = parse_pactl_percent(&out) else { return };
    if (cur as i32 - duck_pct).abs() > 3 {
        return;
    }
    let val = format!("{saved_percent}%");
    let _ = run_quiet(program, &["set-sink-volume", target, &val]).await;
}

// ------------------------------------------------------------------------- MPRIS pause / resume

async fn mpris_status(conn: &zbus::Connection, name: &str) -> Option<String> {
    let msg = tokio::time::timeout(
        MPRIS_TIMEOUT,
        conn.call_method(
            Some(name),
            "/org/mpris/MediaPlayer2",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.mpris.MediaPlayer2.Player", "PlaybackStatus"),
        ),
    )
    .await
    .ok()?
    .ok()?;
    let val = msg.body().deserialize::<zbus::zvariant::OwnedValue>().ok()?;
    match &*val {
        zbus::zvariant::Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

async fn pause_mpris() -> Vec<String> {
    let work = async {
        let Ok(Ok(conn)) = tokio::time::timeout(MPRIS_TIMEOUT, zbus::Connection::session()).await else {
            return Vec::new();
        };
        let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else { return Vec::new() };
        let Ok(Ok(names)) = tokio::time::timeout(MPRIS_TIMEOUT, dbus.list_names()).await else {
            return Vec::new();
        };
        let players: Vec<String> = names
            .into_iter()
            .map(|n| n.to_string())
            .filter(|n| n.starts_with("org.mpris.MediaPlayer2."))
            .collect();
        let results = join_all(players.into_iter().map(|name| {
            let conn = conn.clone();
            async move {
                if mpris_status(&conn, &name).await.as_deref() != Some("Playing") {
                    return None;
                }
                let ok = tokio::time::timeout(
                    MPRIS_TIMEOUT,
                    conn.call_method(Some(name.as_str()), "/org/mpris/MediaPlayer2", Some("org.mpris.MediaPlayer2.Player"), "Pause", &()),
                )
                .await
                .is_ok_and(|r| r.is_ok());
                ok.then_some(name)
            }
        }))
        .await;
        results.into_iter().flatten().collect()
    };
    tokio::time::timeout(MPRIS_BUDGET, work).await.unwrap_or_default()
}

async fn resume_mpris(players: &[String]) {
    let work = async {
        let Ok(Ok(conn)) = tokio::time::timeout(MPRIS_TIMEOUT, zbus::Connection::session()).await else { return };
        let _ = join_all(players.iter().map(|name| {
            let conn = conn.clone();
            async move {
                // Only resume players that are still Paused (respect manual Stop/Play during dictation).
                if mpris_status(&conn, name).await.as_deref() != Some("Paused") {
                    return;
                }
                let _ = tokio::time::timeout(
                    MPRIS_TIMEOUT,
                    conn.call_method(Some(name.as_str()), "/org/mpris/MediaPlayer2", Some("org.mpris.MediaPlayer2.Player"), "Play", &()),
                )
                .await;
            }
        }))
        .await;
    };
    let _ = tokio::time::timeout(MPRIS_BUDGET, work).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::testdir::TestDir;

    #[test]
    fn action_parsing_and_formatting() {
        assert_eq!(MediaAction::parse("keep"), MediaAction::Keep);
        assert_eq!(MediaAction::parse("duck"), MediaAction::Duck);
        assert_eq!(MediaAction::parse("pause"), MediaAction::Pause);
        assert_eq!(MediaAction::parse("unknown"), MediaAction::Keep);
        assert_eq!((MediaAction::Keep.as_str(), MediaAction::Duck.as_str(), MediaAction::Pause.as_str()), ("keep", "duck", "pause"));
    }

    #[test]
    fn volume_and_id_parsers() {
        assert_eq!(parse_wpctl_volume("Volume: 0.85\n"), Some((0.85, false)));
        assert_eq!(parse_wpctl_volume("Volume: 1.00 [MUTED]\n"), Some((1.0, true)));
        assert_eq!(parse_wpctl_volume("Volume: 0,42\n"), Some((0.42, false)));
        assert_eq!(parse_wpctl_volume("broken"), None);
        assert_eq!(parse_wpctl_id("id 64, type PipeWire:Interface:Node\n"), Some("64".into()));
        assert_eq!(parse_wpctl_id("not-an-id"), None);
        assert_eq!(parse_pactl_percent("Volume: front-left: 55705 /  85% / -4.23 dB, front-right: 55705 /  85% / -4.23 dB"), Some(85));
        assert_eq!(parse_pactl_percent("no percent"), None);
    }

    #[tokio::test]
    async fn wpctl_ducks_pinned_sink_and_restores_but_respects_manual_changes_and_mute() {
        let t = TestDir::new("media-wpctl");
        let state = t.path().join("vol");
        let log = t.path().join("log");
        std::fs::write(&state, "Volume: 0.80\n").unwrap();
        let script = format!(
            "#!/bin/sh\nSTATE='{}'\nLOG='{}'\necho \"$*\" >> \"$LOG\"\ncase \"$1\" in\n  inspect) echo 'id 64, type PipeWire:Interface:Node' ;;\n  get-volume) cat \"$STATE\" ;;\n  set-volume) printf 'Volume: %s\\n' \"$3\" > \"$STATE\" ;;\nesac\n",
            state.display(),
            log.display()
        );
        let bin = t.script("wpctl", &script);
        let dirs = Dirs::rooted(t.path(), bin.to_str().unwrap());

        // Normal duck + restore uses pinned node id 64
        let mut g = MediaGuard::acquire(MediaAction::Duck, &dirs).await;
        assert_eq!(std::fs::read_to_string(&state).unwrap().trim(), "Volume: 0.15");
        g.restore().await;
        assert_eq!(std::fs::read_to_string(&state).unwrap().trim(), "Volume: 0.80");
        let recorded = std::fs::read_to_string(&log).unwrap();
        assert!(recorded.contains("set-volume 64 0.15"), "{recorded}");
        assert!(recorded.contains("set-volume 64 0.80"), "{recorded}");

        // User manually changes volume while ducked -> restore leaves their new volume untouched
        let mut g = MediaGuard::acquire(MediaAction::Duck, &dirs).await;
        assert_eq!(std::fs::read_to_string(&state).unwrap().trim(), "Volume: 0.15");
        std::fs::write(&state, "Volume: 0.60\n").unwrap();
        g.restore().await;
        assert_eq!(std::fs::read_to_string(&state).unwrap().trim(), "Volume: 0.60");

        // Already muted -> untouched
        std::fs::write(&state, "Volume: 0.80 [MUTED]\n").unwrap();
        let mut g = MediaGuard::acquire(MediaAction::Duck, &dirs).await;
        assert_eq!(std::fs::read_to_string(&state).unwrap().trim(), "Volume: 0.80 [MUTED]");
        g.restore().await;
        assert_eq!(std::fs::read_to_string(&state).unwrap().trim(), "Volume: 0.80 [MUTED]");

        // Already quieter than duck target -> untouched
        std::fs::write(&state, "Volume: 0.10\n").unwrap();
        let mut g = MediaGuard::acquire(MediaAction::Duck, &dirs).await;
        assert_eq!(std::fs::read_to_string(&state).unwrap().trim(), "Volume: 0.10");
        g.restore().await;
        assert_eq!(std::fs::read_to_string(&state).unwrap().trim(), "Volume: 0.10");
    }

    #[tokio::test]
    async fn pactl_fallback_ducks_pinned_sink_and_restores() {
        let t = TestDir::new("media-pactl");
        let state = t.path().join("vol");
        std::fs::write(&state, "75").unwrap();
        let script = format!(
            "#!/bin/sh\nSTATE='{}'\ncase \"$1\" in\n  get-default-sink) echo 'alsa_output.test' ;;\n  get-sink-mute) echo 'Mute: no' ;;\n  get-sink-volume) printf 'Volume: front-left: 1 / %s%% / 0 dB\\n' \"$(cat \"$STATE\")\" ;;\n  set-sink-volume) printf '%s' \"${{3%%%}}\" > \"$STATE\" ;;\nesac\n",
            state.display()
        );
        let bin = t.script("pactl", &script);
        let dirs = Dirs::rooted(t.path(), bin.to_str().unwrap());

        let mut g = MediaGuard::acquire(MediaAction::Duck, &dirs).await;
        assert_eq!(std::fs::read_to_string(&state).unwrap(), "15");
        g.restore().await;
        assert_eq!(std::fs::read_to_string(&state).unwrap(), "75");
    }
}
