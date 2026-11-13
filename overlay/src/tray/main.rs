//! maprisma-dictation-tray: the SNI tray icon for KDE Plasma and Maprisma Shell on Hyprland
//! (COSMIC has the applet). Left click toggles the overlay (`maprisma-dictation --toggle`),
//! right click shows the menu: open/close dictation, Voice settings…, Quit tray. The icon follows
//! the overlay and the voice service (src/bar.rs).
use maprisma_dictation::bar::{self, Command, Shown};
use maprisma_voice::i18n::tr;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::{Mutex, mpsc};
use zbus::{
    Connection,
    object_server::SignalEmitter,
    zvariant::{OwnedObjectPath, OwnedValue, Value as Variant},
};

type Props = HashMap<String, OwnedValue>;
type Layout = (i32, Props, Vec<OwnedValue>);
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    /// Left click or the first menu entry: the overlay opens (and listens), or closes.
    Toggle,
    Settings,
    Quit,
}

#[derive(Clone, Debug, PartialEq)]
struct Item {
    id: i32,
    label: String,
    separator: bool,
    children: Vec<Item>,
    action: Option<Action>,
}

impl Item {
    fn find(&self, id: i32) -> Option<&Self> {
        if self.id == id { Some(self) } else { self.children.iter().find_map(|c| c.find(id)) }
    }
}

fn menu(shown: &Shown) -> Item {
    let item = |id, label: String, action| Item { id, label, separator: false, children: vec![], action };
    let mut separator = item(3, String::new(), None);
    separator.separator = true;
    let mut root = item(0, String::new(), None);
    root.children = vec![
        item(1, shown.toggle_label(), Some(Action::Toggle)),
        item(2, tr("Voice settings…"), Some(Action::Settings)),
        separator,
        item(4, tr("Quit tray"), Some(Action::Quit)),
    ];
    root
}

struct State {
    shown: Shown,
    menu: Item,
    revision: u32,
}
type Shared = Arc<Mutex<State>>;

fn owned<T: Into<Variant<'static>>>(value: T) -> OwnedValue {
    OwnedValue::try_from(value.into()).unwrap()
}

fn properties(item: &Item, names: &[String]) -> Props {
    let mut props = Props::new();
    props.insert("label".into(), owned(item.label.clone()));
    props.insert("enabled".into(), owned(true));
    props.insert("visible".into(), owned(true));
    if item.separator {
        props.insert("type".into(), owned("separator"));
    }
    if !names.is_empty() {
        props.retain(|key, _| names.contains(key));
    }
    props
}

fn layout(item: &Item, depth: i32, names: &[String]) -> Layout {
    let children = if depth == 0 {
        vec![]
    } else {
        item.children.iter().map(|c| OwnedValue::try_from(Variant::new(layout(c, if depth < 0 { -1 } else { depth - 1 }, names))).unwrap()).collect()
    };
    (item.id, properties(item, names), children)
}

#[derive(Clone)]
struct Menu {
    state: Shared,
    tx: mpsc::UnboundedSender<Action>,
}

impl Menu {
    async fn dispatch(&self, id: i32, event: &str) -> bool {
        let s = self.state.lock().await;
        let Some(item) = s.menu.find(id) else { return false };
        if event == "clicked" {
            if let Some(action) = item.action {
                let _ = self.tx.send(action);
            }
        }
        true
    }
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        vec![]
    }
    async fn get_layout(&self, parent_id: i32, recursion_depth: i32, property_names: Vec<String>) -> zbus::fdo::Result<(u32, Layout)> {
        let s = self.state.lock().await;
        let item = s.menu.find(parent_id).ok_or_else(|| zbus::fdo::Error::InvalidArgs("unknown menu id".into()))?;
        Ok((s.revision, layout(item, recursion_depth, &property_names)))
    }
    async fn get_group_properties(&self, ids: Vec<i32>, property_names: Vec<String>) -> Vec<(i32, Props)> {
        let s = self.state.lock().await;
        ids.iter().filter_map(|id| s.menu.find(*id).map(|i| (*id, properties(i, &property_names)))).collect()
    }
    async fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedValue> {
        let s = self.state.lock().await;
        s.menu.find(id).and_then(|i| properties(i, &[]).remove(&name)).ok_or_else(|| zbus::fdo::Error::InvalidArgs("unknown menu property".into()))
    }
    async fn event(&self, id: i32, event_id: String, _data: OwnedValue, _timestamp: u32) {
        self.dispatch(id, &event_id).await;
    }
    async fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        let mut errors = vec![];
        for (id, event, _, _) in events {
            if !self.dispatch(id, &event).await {
                errors.push(id);
            }
        }
        errors
    }
    async fn about_to_show(&self, _id: i32) -> bool {
        false
    }
    async fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        let s = self.state.lock().await;
        (vec![], ids.into_iter().filter(|id| s.menu.find(*id).is_none()).collect())
    }
    #[zbus(signal)]
    async fn layout_updated(emitter: &SignalEmitter<'_>, revision: u32, parent: i32) -> zbus::Result<()>;
}

struct Tray {
    state: Shared,
    tx: mpsc::UnboundedSender<Action>,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Tray {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        bar::SNI_ID
    }
    /// Starts with "Maprisma" in every language: the KDE "Maprisma Icons" widget picks it up.
    #[zbus(property)]
    fn title(&self) -> String {
        tr("Maprisma Dictation")
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }
    #[zbus(property)]
    fn window_id(&self) -> u32 {
        0
    }
    #[zbus(property)]
    async fn icon_name(&self) -> &str {
        self.state.lock().await.shown.icon()
    }
    #[zbus(property)]
    fn icon_pixmap(&self) -> Pixmaps {
        vec![]
    }
    /// Where the icons are when they aren't in the icon theme's search path (a prefix install).
    #[zbus(property)]
    fn icon_theme_path(&self) -> String {
        bar::icon_theme_path(std::env::current_exe().ok().as_deref())
    }
    #[zbus(property)]
    fn overlay_icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn overlay_icon_pixmap(&self) -> Pixmaps {
        vec![]
    }
    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        bar::ICON_LISTENING
    }
    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Pixmaps {
        vec![]
    }
    #[zbus(property)]
    fn attention_movie_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    async fn tool_tip(&self) -> (String, Pixmaps, String, String) {
        let s = self.state.lock().await;
        (s.shown.icon().into(), vec![], tr("Maprisma Dictation"), s.shown.status())
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/MenuBar").unwrap()
    }
    fn activate(&self, _x: i32, _y: i32) {
        let _ = self.tx.send(Action::Toggle);
    }
    // Middle click: nothing of its own (the left click already does the one thing).
    fn secondary_activate(&self, _x: i32, _y: i32) {}
    fn context_menu(&self, _x: i32, _y: i32) {} // The host shows the exported menu.
    fn scroll(&self, _delta: i32, _orientation: String) {}
    #[zbus(signal)]
    async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// The overlay's language setting (re-read: the Manager's Voice page can change it any time).
fn set_language() {
    maprisma_voice::i18n::set_language(&maprisma_dictation::settings::read().language);
}

/// Shows `shown` (or the same again in a new language): icon, tooltip, menu.
async fn refresh(state: &Shared, connection: &Connection, shown: Shown) -> zbus::Result<()> {
    let mut s = state.lock().await;
    let menu = menu(&shown);
    if s.menu != menu {
        s.menu = menu;
        s.revision += 1;
        Menu::layout_updated(&SignalEmitter::new(connection, "/MenuBar")?, s.revision, 0).await?;
    }
    let icon = s.shown.icon() != shown.icon();
    let changed = s.shown != shown;
    s.shown = shown;
    drop(s);
    let emitter = SignalEmitter::new(connection, "/StatusNotifierItem")?;
    if icon {
        Tray::new_icon(&emitter).await?;
    }
    if changed {
        Tray::new_tool_tip(&emitter).await?;
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    set_language();
    let state = Arc::new(Mutex::new(State { shown: Shown::default(), menu: menu(&Shown::default()), revision: 1 }));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let connection = zbus::connection::Builder::session()?
        .serve_at("/StatusNotifierItem", Tray { state: state.clone(), tx: tx.clone() })?
        .serve_at("/MenuBar", Menu { state: state.clone(), tx })?
        .build()
        .await?;
    // One tray per session: a second one ends here (Builder::name would queue the request).
    match connection.request_name_with_flags(bar::TRAY_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into()).await {
        Ok(_) => (),
        Err(zbus::Error::NameTaken) => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    // Register with the watcher, again whenever it changes (Plasma or the shell restarted).
    let watcher_connection = connection.clone();
    tokio::spawn(async move {
        let mut registered = String::new();
        loop {
            let owner = match zbus::fdo::DBusProxy::new(&watcher_connection).await {
                Ok(p) => p.get_name_owner("org.kde.StatusNotifierWatcher".try_into().unwrap()).await.map(|s| s.to_string()).unwrap_or_default(),
                Err(_) => String::new(),
            };
            if owner.is_empty() {
                registered.clear();
            } else if owner != registered {
                if let Ok(proxy) = zbus::Proxy::new(&watcher_connection, owner.as_str(), "/StatusNotifierWatcher", "org.kde.StatusNotifierWatcher").await {
                    if proxy.call::<_, _, ()>("RegisterStatusNotifierItem", &(watcher_connection.unique_name().unwrap().as_str(),)).await.is_ok() {
                        registered = owner;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    // The overlay's and the voice service's state, from the bus.
    let (shown_tx, mut shown_rx) = mpsc::unbounded_channel();
    let watch_connection = connection.clone();
    tokio::spawn(async move {
        loop {
            let _ = bar::watch(&watch_connection, |shown| {
                let _ = shown_tx.send(shown);
            })
            .await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    let mut language = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            Some(shown) = shown_rx.recv() => refresh(&state, &connection, shown).await?,
            _ = language.tick() => {
                set_language();
                let shown = state.lock().await.shown;
                refresh(&state, &connection, shown).await?;
            }
            Some(action) = rx.recv() => match action {
                Action::Toggle => {
                    if let Err(e) = Command::toggle().spawn() {
                        eprintln!("maprisma-dictation-tray: could not start maprisma-dictation: {e}");
                    }
                }
                Action::Settings => {
                    if let Err(e) = bar::settings_command().spawn() {
                        eprintln!("maprisma-dictation-tray: could not start maprisma-settings: {e}");
                    }
                }
                Action::Quit => return Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_entries() {
        maprisma_dictation::paths::test_roots();
        maprisma_voice::i18n::set_language("en");
        let m = menu(&Shown::default());
        let labels: Vec<&str> = m.children.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Open dictation", "Voice settings…", "", "Quit tray"]);
        assert_eq!(m.find(1).unwrap().action, Some(Action::Toggle));
        assert_eq!(m.find(2).unwrap().action, Some(Action::Settings));
        assert!(m.find(3).unwrap().separator);
        assert_eq!(m.find(4).unwrap().action, Some(Action::Quit));
        assert!(m.find(99).is_none());
        assert_eq!(menu(&Shown { open: true, ..Default::default() }).find(1).unwrap().label, "Close dictation");
    }

    #[test]
    fn dbus_layout() {
        maprisma_dictation::paths::test_roots();
        let m = menu(&Shown::default());
        assert_eq!(Variant::new(layout(&m, -1, &[])).value_signature().to_string(), "(ia{sv}av)");
        assert!(layout(&m, 0, &[]).2.is_empty());
        assert_eq!(layout(&m, 1, &[]).2.len(), 4);
        assert_eq!(properties(m.find(1).unwrap(), &["label".into()]).len(), 1);
    }

    #[tokio::test]
    async fn menu_events() {
        maprisma_dictation::paths::test_roots();
        let shown = Shown::default();
        let state = Arc::new(Mutex::new(State { shown, menu: menu(&shown), revision: 1 }));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let m = Menu { state, tx };
        m.event(1, "clicked".into(), owned(0i32), 0).await;
        assert_eq!(rx.try_recv().unwrap(), Action::Toggle);
        m.event(2, "hovered".into(), owned(0i32), 0).await;
        assert!(rx.try_recv().is_err());
        assert_eq!(m.event_group(vec![(99, "clicked".into(), owned(0i32), 0), (2, "clicked".into(), owned(0i32), 0)]).await, vec![99]);
        assert_eq!(rx.try_recv().unwrap(), Action::Settings);
    }
}
