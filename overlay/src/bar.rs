//! The bar icon that toggles the overlay (family rule "bar-icon toggle"): the COSMIC panel applet
//! (`maprisma-dictation-applet`) and the StatusNotifierItem tray for KDE Plasma and Maprisma
//! Shell on Hyprland (`maprisma-dictation-tray`). Both run `maprisma-dictation --toggle`, the
//! overlay's own toggle, so the bar, a key binding and the Launcher take the same path.
//!
//! The icon shows what happens, from the session bus only (nothing is started for it):
//! - the microphone is on (`com.maprisma.Voice1` `State`: listening, transcribing) → mic with
//!   sound arcs; this is any app's voice session, as only one listens at a time;
//! - an answer is read aloud (`Speaking`) → filled mic with a dot;
//! - the overlay is open (`com.maprisma.Dictation` has an owner) → filled mic;
//! - else the mic outline.
//!
//! `Status` is asked only when the voice service already runs (never D-Bus-activated from here):
//! once at the start, when it appears, and every 15 s while the icon shows it busy (a missed
//! end signal can't leave the icon stuck).

use futures_util::StreamExt;
use maprisma_voice::i18n::tr;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const APP_ID: &str = "io.github.ubruckhaus.CosmicMaprismaDictation";
/// The tray's own bus name (one tray per session).
pub const TRAY_NAME: &str = "io.github.ubruckhaus.CosmicMaprismaDictation.Tray";
/// The StatusNotifierItem `Id` (Maprisma Manager's `kdeId`, Maprisma Shell's `tray:<id>`).
pub const SNI_ID: &str = "maprisma-dictation";

pub const ICON: &str = "io.github.ubruckhaus.CosmicMaprismaDictation-symbolic";
pub const ICON_OPEN: &str = "io.github.ubruckhaus.CosmicMaprismaDictation-open-symbolic";
pub const ICON_LISTENING: &str = "io.github.ubruckhaus.CosmicMaprismaDictation-listening-symbolic";
pub const ICON_SPEAKING: &str = "io.github.ubruckhaus.CosmicMaprismaDictation-speaking-symbolic";

/// What the voice service does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Voice {
    #[default]
    Idle,
    Listening,
    Transcribing,
}

/// What the bar icon shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shown {
    /// The overlay is open.
    pub open: bool,
    pub voice: Voice,
    /// An answer is read aloud.
    pub speaking: bool,
}

/// What arrives from the bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// `State(session, state)`: listening, transcribing, done, cancelled, error.
    State(String),
    /// `Speaking(speak_id, state, message)`: speaking, done, stopped, error.
    Speaking(String),
    /// `Closed(session, reason)`: the session is over, whatever the reason.
    Closed,
    /// `com.maprisma.Dictation` got (true) or lost (false) its owner.
    Overlay(bool),
    /// `com.maprisma.Voice1` got or lost its owner.
    Service(bool),
    /// The answer of `Status`: `state` and `speaking`.
    Status { state: String, speaking: bool },
}

impl Shown {
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::State(state) => self.voice = voice_of(state).unwrap_or(self.voice),
            Event::Speaking(state) => self.speaking = state == "speaking",
            Event::Closed => self.voice = Voice::Idle,
            Event::Overlay(open) => self.open = *open,
            Event::Service(true) => {}
            Event::Service(false) => {
                self.voice = Voice::Idle;
                self.speaking = false;
            }
            Event::Status { state, speaking } => {
                // Status knows only idle, listening and transcribing.
                self.voice = voice_of(state).unwrap_or(Voice::Idle);
                self.speaking = *speaking;
            }
        }
    }

    /// The voice service is doing something (listening, transcribing or speaking).
    pub fn busy(&self) -> bool {
        self.voice != Voice::Idle || self.speaking
    }

    pub fn icon(&self) -> &'static str {
        if self.voice != Voice::Idle {
            ICON_LISTENING
        } else if self.speaking {
            ICON_SPEAKING
        } else if self.open {
            ICON_OPEN
        } else {
            ICON
        }
    }

    /// The status line (tray tooltip, the applet's popup).
    pub fn status(&self) -> String {
        match (self.voice, self.speaking, self.open) {
            (Voice::Listening, ..) => tr("Listening…"),
            (Voice::Transcribing, ..) => tr("Transcribing…"),
            (_, true, _) => tr("Reading the answer aloud"),
            (_, _, true) => tr("Dictation is open"),
            _ => tr("Click to speak or type a question"),
        }
    }

    /// The menu entry that toggles the overlay.
    pub fn toggle_label(&self) -> String {
        if self.open { tr("Close dictation") } else { tr("Open dictation") }
    }
}

/// A `State` value; None for one this doesn't know (the icon keeps what it shows).
fn voice_of(state: &str) -> Option<Voice> {
    match state {
        "listening" => Some(Voice::Listening),
        "transcribing" => Some(Voice::Transcribing),
        "idle" | "done" | "cancelled" | "error" => Some(Voice::Idle),
        _ => None,
    }
}

// ------------------------------------------------------------------------- commands

/// The overlay's program: the one next to this bar program (an install puts them together),
/// else `~/.local/bin/maprisma-dictation`, else from PATH.
pub fn overlay_program(exe: Option<&Path>, home: &Path) -> PathBuf {
    if let Some(sibling) = exe.and_then(Path::parent).map(|dir| dir.join("maprisma-dictation")) {
        if sibling.is_file() {
            return sibling;
        }
    }
    let local = home.join(".local/bin/maprisma-dictation");
    if local.is_file() { local } else { PathBuf::from("maprisma-dictation") }
}

/// A command as program and arguments (never a shell).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// Click on the bar icon: the overlay's own toggle (it opens and listens, or closes).
pub fn toggle_command(exe: Option<&Path>, home: &Path) -> Command {
    Command { program: overlay_program(exe, home), args: vec!["--toggle".into()] }
}

/// "Voice settings…": Maprisma Manager's Voice page.
pub fn settings_command() -> Command {
    Command { program: PathBuf::from("maprisma-manager"), args: vec!["--page".into(), "voice".into()] }
}

impl Command {
    /// For this program, in this session.
    pub fn toggle() -> Self {
        toggle_command(std::env::current_exe().ok().as_deref(), &crate::paths::home())
    }

    /// Starts it in the background (stdin closed); a thread reaps it.
    pub fn spawn(&self) -> std::io::Result<()> {
        let mut child = std::process::Command::new(&self.program).args(&self.args).stdin(std::process::Stdio::null()).spawn()?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

// ------------------------------------------------------------------------- the bus

pub const OVERLAY_NAME: &str = crate::instance::NAME;
use maprisma_voice::dbus::{INTERFACE as VOICE_INTERFACE, NAME as VOICE_NAME, PATH as VOICE_PATH};

/// One signal of interest, decoded (None for anything else).
pub fn decode(message: &zbus::Message) -> Option<Event> {
    let header = message.header();
    let member = header.member()?.as_str().to_string();
    let body = message.body();
    match member.as_str() {
        "State" => body.deserialize::<(String, String)>().ok().map(|(_, state)| Event::State(state)),
        "Speaking" => body.deserialize::<(String, String, String)>().ok().map(|(_, state, _)| Event::Speaking(state)),
        "Closed" => body.deserialize::<(String, String)>().ok().map(|_| Event::Closed),
        "NameOwnerChanged" => body.deserialize::<(String, String, String)>().ok().and_then(|(name, _, owner)| match name.as_str() {
            OVERLAY_NAME => Some(Event::Overlay(!owner.is_empty())),
            VOICE_NAME => Some(Event::Service(!owner.is_empty())),
            _ => None,
        }),
        _ => None,
    }
}

/// `Status` of a running service; None when it doesn't run (it is not started for this).
async fn status(connection: &zbus::Connection, dbus: &zbus::fdo::DBusProxy<'_>) -> Option<Event> {
    if !dbus.name_has_owner(VOICE_NAME.try_into().ok()?).await.unwrap_or(false) {
        return None;
    }
    let reply = connection.call_method(Some(VOICE_NAME), VOICE_PATH, Some(VOICE_INTERFACE), "Status", &()).await.ok()?;
    let map: HashMap<String, zbus::zvariant::OwnedValue> = reply.body().deserialize().ok()?;
    Some(status_event(&map))
}

pub fn status_event(map: &HashMap<String, zbus::zvariant::OwnedValue>) -> Event {
    use zbus::zvariant::Value;
    let state = map.get("state").and_then(|v| if let Value::Str(s) = &**v { Some(s.to_string()) } else { None }).unwrap_or_default();
    let speaking = map.get("speaking").is_some_and(|v| matches!(&**v, Value::Bool(true)));
    Event::Status { state, speaking }
}

/// Follows the overlay and the voice service and calls `changed` with each new [Shown] (the
/// first right away). Returns only when the bus fails.
pub async fn watch(connection: &zbus::Connection, mut changed: impl FnMut(Shown)) -> zbus::Result<()> {
    use zbus::{MatchRule, MessageStream, message::Type};
    let dbus = zbus::fdo::DBusProxy::new(connection).await?;
    let mut rules = vec![];
    // Not `Level` (20 a second while listening) and not `Partial`.
    for member in ["State", "Speaking", "Closed"] {
        rules.push(MatchRule::builder().msg_type(Type::Signal).interface(VOICE_INTERFACE)?.path(VOICE_PATH)?.member(member)?.build());
    }
    for name in [OVERLAY_NAME, VOICE_NAME] {
        rules.push(
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender("org.freedesktop.DBus")?
                .interface("org.freedesktop.DBus")?
                .member("NameOwnerChanged")?
                .add_arg(name)?
                .build(),
        );
    }
    let mut streams = vec![];
    for rule in rules {
        streams.push(MessageStream::for_match_rule(rule, connection, Some(64)).await?);
    }
    let mut messages = futures_util::stream::select_all(streams);

    let mut shown = Shown::default();
    shown.apply(&Event::Overlay(dbus.name_has_owner(OVERLAY_NAME.try_into().unwrap()).await.unwrap_or(false)));
    if let Some(event) = status(connection, &dbus).await {
        shown.apply(&event);
    }
    changed(shown);
    let mut check = tokio::time::interval(Duration::from_secs(15));
    check.tick().await;
    loop {
        let event = tokio::select! {
            message = messages.next() => match message {
                Some(Ok(message)) => decode(&message),
                Some(Err(e)) => return Err(e),
                None => return Ok(()),
            },
            _ = check.tick() => if shown.busy() { status(connection, &dbus).await.or(Some(Event::Service(false))) } else { None },
        };
        let Some(event) = event else { continue };
        let before = shown;
        shown.apply(&event);
        // The service (re)started: ask what it does.
        if event == Event::Service(true) {
            if let Some(status) = status(connection, &dbus).await {
                shown.apply(&status);
            }
        }
        if shown != before {
            changed(shown);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(events: &[Event]) -> Shown {
        let mut s = Shown::default();
        for e in events {
            s.apply(e);
        }
        s
    }

    #[test]
    fn signals_to_icon_state() {
        crate::paths::test_roots();
        let st = |s: &str| Event::State(s.into());
        assert_eq!(after(&[]).icon(), ICON);
        assert_eq!(after(&[Event::Overlay(true)]).icon(), ICON_OPEN);
        assert_eq!(after(&[Event::Overlay(true), st("listening")]).icon(), ICON_LISTENING);
        assert_eq!(after(&[st("listening"), st("transcribing")]).voice, Voice::Transcribing);
        assert_eq!(after(&[st("listening"), st("transcribing")]).icon(), ICON_LISTENING);
        // The end of a session, however it ends.
        for end in ["done", "cancelled", "error"] {
            assert_eq!(after(&[Event::Overlay(true), st("listening"), st(end)]).icon(), ICON_OPEN, "{end}");
        }
        assert_eq!(after(&[st("listening"), Event::Closed]).voice, Voice::Idle);
        // An unknown state keeps what shows.
        assert_eq!(after(&[st("listening"), st("warming-up")]).voice, Voice::Listening);
        // Spoken answers.
        let sp = |s: &str| Event::Speaking(s.into());
        assert_eq!(after(&[Event::Overlay(true), sp("speaking")]).icon(), ICON_SPEAKING);
        for end in ["done", "stopped", "error"] {
            assert!(!after(&[sp("speaking"), sp(end)]).speaking, "{end}");
        }
        // Listening wins over speaking (a new session stops the speech anyway).
        assert_eq!(after(&[sp("speaking"), st("listening")]).icon(), ICON_LISTENING);
        // The overlay closes; the service goes away mid-session.
        assert_eq!(after(&[Event::Overlay(true), Event::Overlay(false)]).icon(), ICON);
        let gone = after(&[Event::Overlay(true), st("listening"), sp("speaking"), Event::Service(false)]);
        assert_eq!((gone.voice, gone.speaking, gone.icon()), (Voice::Idle, false, ICON_OPEN));
        assert_eq!(after(&[st("listening"), Event::Service(true)]).voice, Voice::Listening);
        // Status replaces what the signals said.
        let status = |state: &str, speaking| Event::Status { state: state.into(), speaking };
        assert_eq!(after(&[st("listening"), status("idle", true)]).icon(), ICON_SPEAKING);
        assert_eq!(after(&[status("transcribing", false)]).voice, Voice::Transcribing);
        assert_eq!(after(&[st("listening"), status("", false)]).voice, Voice::Idle);
        assert!(after(&[status("listening", false)]).busy());
        assert!(!after(&[Event::Overlay(true)]).busy());
    }

    #[test]
    fn texts() {
        crate::paths::test_roots();
        maprisma_voice::i18n::set_language("en");
        let mut s = Shown::default();
        assert_eq!((s.status().as_str(), s.toggle_label().as_str()), ("Click to speak or type a question", "Open dictation"));
        s.open = true;
        assert_eq!((s.status().as_str(), s.toggle_label().as_str()), ("Dictation is open", "Close dictation"));
        s.speaking = true;
        assert_eq!(s.status(), "Reading the answer aloud");
        s.voice = Voice::Transcribing;
        assert_eq!(s.status(), "Transcribing…");
        s.voice = Voice::Listening;
        assert_eq!(s.status(), "Listening…");
    }

    struct FakeVoice {
        state: String,
    }

    #[zbus::interface(name = "com.maprisma.Voice1")]
    impl FakeVoice {
        fn status(&self) -> HashMap<String, zbus::zvariant::OwnedValue> {
            let mut map = HashMap::new();
            map.insert("state".into(), zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(self.state.clone())).unwrap());
            map.insert("speaking".into(), zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(false)).unwrap());
            map
        }
        #[zbus(signal)]
        async fn state(emitter: &zbus::object_server::SignalEmitter<'_>, session: &str, state: &str) -> zbus::Result<()>;
        #[zbus(signal)]
        async fn speaking(emitter: &zbus::object_server::SignalEmitter<'_>, id: &str, state: &str, message: &str) -> zbus::Result<()>;
        #[zbus(signal)]
        async fn level(emitter: &zbus::object_server::SignalEmitter<'_>, session: &str, rms: f64) -> zbus::Result<()>;
    }

    /// The watcher on a private bus: the overlay's name, a (fake) voice service's Status and signals.
    #[tokio::test]
    #[ignore = "needs a session bus: dbus-run-session -- cargo test -p maprisma-dictation -- --ignored"]
    async fn watch_on_a_bus() {
        let _turn = crate::instance::TEST_NAME_LOCK.lock().await;
        let next = async |rx: &mut tokio::sync::mpsc::UnboundedReceiver<Shown>| tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        let watcher = zbus::Connection::session().await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move { watch(&watcher, |s| {
            let _ = tx.send(s);
        })
        .await });
        assert_eq!(next(&mut rx).await, Shown::default());
        // The overlay opens and closes.
        let overlay = zbus::connection::Builder::session().unwrap().name(OVERLAY_NAME).unwrap().build().await.unwrap();
        assert_eq!(next(&mut rx).await.icon(), ICON_OPEN);
        // The voice service starts while a session listens: Status tells.
        let voice = zbus::connection::Builder::session().unwrap().serve_at(VOICE_PATH, FakeVoice { state: "listening".into() }).unwrap().name(VOICE_NAME).unwrap().build().await.unwrap();
        assert_eq!(next(&mut rx).await.voice, Voice::Listening);
        let emitter = zbus::object_server::SignalEmitter::new(&voice, VOICE_PATH).unwrap();
        // Levels change nothing (and aren't even delivered); transcribing, done, speaking do.
        FakeVoice::level(&emitter, "s1", 0.5).await.unwrap();
        FakeVoice::state(&emitter, "s1", "transcribing").await.unwrap();
        assert_eq!(next(&mut rx).await.voice, Voice::Transcribing);
        FakeVoice::state(&emitter, "s1", "done").await.unwrap();
        assert_eq!(next(&mut rx).await, Shown { open: true, ..Default::default() });
        FakeVoice::speaking(&emitter, "k1", "speaking", "").await.unwrap();
        assert_eq!(next(&mut rx).await.icon(), ICON_SPEAKING);
        // The service goes away while speaking; then the overlay closes.
        drop(emitter);
        drop(voice);
        assert_eq!(next(&mut rx).await.icon(), ICON_OPEN);
        drop(overlay);
        assert_eq!(next(&mut rx).await, Shown::default());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn status_reply() {
        use zbus::zvariant::{OwnedValue, Value};
        let mut map: HashMap<String, OwnedValue> = HashMap::new();
        map.insert("state".into(), OwnedValue::try_from(Value::from("listening")).unwrap());
        map.insert("speaking".into(), OwnedValue::try_from(Value::from(true)).unwrap());
        assert_eq!(status_event(&map), Event::Status { state: "listening".into(), speaking: true });
        assert_eq!(status_event(&HashMap::new()), Event::Status { state: String::new(), speaking: false });
    }

    #[test]
    fn toggle_command_building() {
        let root = crate::paths::test_roots().join("bar-commands");
        let bin = root.join("prefix/bin");
        let home = root.join("home");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        let tray = bin.join("maprisma-dictation-tray");
        // Nothing installed: from PATH.
        assert_eq!(toggle_command(Some(&tray), &home), Command { program: "maprisma-dictation".into(), args: vec!["--toggle".into()] });
        assert_eq!(toggle_command(None, &home).program, PathBuf::from("maprisma-dictation"));
        // ~/.local/bin.
        std::fs::write(home.join(".local/bin/maprisma-dictation"), "").unwrap();
        assert_eq!(toggle_command(Some(&tray), &home).program, home.join(".local/bin/maprisma-dictation"));
        // Next to the bar program wins (a folder of that name doesn't count).
        std::fs::create_dir_all(root.join("other/maprisma-dictation")).unwrap();
        assert_eq!(toggle_command(Some(&root.join("other/x")), &home).program, home.join(".local/bin/maprisma-dictation"));
        std::fs::write(bin.join("maprisma-dictation"), "").unwrap();
        assert_eq!(toggle_command(Some(&tray), &home), Command { program: bin.join("maprisma-dictation"), args: vec!["--toggle".into()] });
        assert_eq!(settings_command(), Command { program: "maprisma-manager".into(), args: vec!["--page".into(), "voice".into()] });
    }

    #[test]
    fn desktop_files() {
        let applet = include_str!("../../data/io.github.ubruckhaus.CosmicMaprismaDictation.desktop");
        let lines: Vec<&str> = applet.lines().collect();
        for line in ["X-CosmicApplet=true", "X-HostWaylandDisplay=true", "NoDisplay=true", "Exec=maprisma-dictation-applet", "Icon=io.github.ubruckhaus.CosmicMaprismaDictation-symbolic", "Name=Maprisma Dictation"] {
            assert!(lines.contains(&line), "applet: {line}");
        }
        let tray = include_str!("../../data/io.github.ubruckhaus.CosmicMaprismaDictation.Tray.desktop");
        let lines: Vec<&str> = tray.lines().collect();
        for line in ["Exec=maprisma-dictation-tray", "OnlyShowIn=KDE;Hyprland;", "Icon=io.github.ubruckhaus.CosmicMaprismaDictation-symbolic", "Type=Application"] {
            assert!(lines.contains(&line), "tray: {line}");
        }
        // Both name and describe it in all 8 languages.
        for file in [applet, tray] {
            for code in ["de", "fr", "es", "it", "pt", "nl", "pl"] {
                assert!(file.lines().any(|l| l.starts_with(&format!("Name[{code}]="))), "{code}");
            }
        }
        assert!(applet.lines().filter(|l| l.starts_with("Comment[")).count() == 7);
        // Every icon the bar shows ships, symbolic (currentColor + KDE's ColorScheme-Text).
        for icon in [ICON, ICON_OPEN, ICON_LISTENING, ICON_SPEAKING] {
            let svg = std::fs::read_to_string(format!("../data/icons/{icon}.svg")).unwrap_or_else(|_| panic!("{icon}"));
            assert!(svg.contains("currentColor") && svg.contains("ColorScheme-Text"), "{icon}");
        }
    }
}
