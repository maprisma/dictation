//! maprisma-dictation-applet: Maprisma Dictation's icon in the COSMIC panel. A click opens the
//! overlay (it listens at once) or closes it: `maprisma-dictation --toggle`, the overlay's own
//! toggle. A right click opens a small menu: open/close dictation, Voice settings… (Maprisma
//! Manager's Voice page), and a gear in its header for the settings page (Language, F12,
//! Corners). The icon follows the overlay and the voice service (src/bar.rs).

use maprisma_dictation::bar::{self, Command, Shown};
use maprisma_dictation::{settings, skin};
use maprisma_kit::{corners, look};
use maprisma_voice::i18n::{LANGUAGES, tr, trf};
use std::sync::LazyLock;
use std::time::Duration;

use cosmic::app::{Core, Task};
use cosmic::applet::cosmic_panel_config::PanelAnchor;
use cosmic::applet::{menu_button, padded_control};
use cosmic::iced::platform_specific::shell::wayland::commands::popup::destroy_popup;
use cosmic::iced::widget::svg as iced_svg;
use cosmic::iced::widget::text as iced_text;
use cosmic::iced::{Alignment, Color, Length, Rectangle, Subscription, window};
use cosmic::widget::rectangle_tracker::{RectangleTracker, RectangleUpdate, rectangle_tracker_subscription};
use cosmic::widget::{Id, autosize, button, column, divider, icon, mouse_area, row, scrollable, space, text};
use cosmic::{Element, theme};

static AUTOSIZE_ID: LazyLock<Id> = LazyLock::new(|| Id::new("dictation-autosize"));
const POPUP_WIDTH: f32 = 300.0;

fn main() -> cosmic::iced::Result {
    look::keep_fonts();
    cosmic::applet::run::<Applet>(())
}

#[derive(Default)]
struct Applet {
    core: Core,
    shown: Shown,
    corners: String,
    skin: String,
    language: String,
    screenshot_key: bool,
    // Shared maprisma-voice settings (~/.config/maprisma-voice/settings.json).
    voice_language: String,
    noise_filter: String,
    vad_sensitivity: String,
    hallucination_filter: bool,
    popup: Option<window::Id>,
    /// The settings page is shown instead of the menu.
    settings_open: bool,
    language_open: bool,
    corners_open: bool,
    skin_open: bool,
    voice_language_open: bool,
    noise_filter_open: bool,
    vad_sensitivity_open: bool,
    setting_error: bool,
    rectangle: Rectangle,
    tracker: Option<RectangleTracker<u32>>,
}

#[derive(Debug, Clone)]
enum Message {
    /// Left click: the overlay opens or closes.
    Toggle,
    /// Right click: the menu.
    TogglePopup,
    /// "Open dictation" / "Close dictation" in the menu.
    MenuToggle,
    Settings,
    ToggleSettings,
    ToggleLanguage,
    ToggleCorners,
    ToggleSkin,
    ToggleVoiceLanguage,
    ToggleNoiseFilter,
    ToggleVadSensitivity,
    Language(String),
    Corners(String),
    SetSkin(String),
    SetShotKey(bool),
    SetVoiceLanguage(String),
    SetNoiseFilter(String),
    SetVadSensitivity(String),
    SetHallucinationFilter(bool),
    Shown(Shown),
    Tick,
    Escape(window::Id),
    Closed(window::Id),
    Rectangle(RectangleUpdate<u32>),
}

/// The bus, as a subscription: every new state of the overlay and the voice service.
fn watch() -> Subscription<Message> {
    Subscription::run(|| {
        cosmic::iced::stream::channel(8, |mut output: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            loop {
                if let Ok(connection) = zbus::Connection::session().await {
                    let _ = bar::watch(&connection, |shown| {
                        let _ = output.try_send(Message::Shown(shown));
                    })
                    .await;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        })
    })
}

/// What Esc does next (layout rule 5a): an open list first, then Back from the settings page,
/// then the popup closes. `shown`: Esc came from the open popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscStep {
    Menus,
    Back,
    Close,
    Nothing,
}

fn esc_step(menus: bool, settings: bool, shown: bool) -> EscStep {
    if !shown {
        EscStep::Nothing
    } else if menus {
        EscStep::Menus
    } else if settings {
        EscStep::Back
    } else {
        EscStep::Close
    }
}

fn dim_text(theme: &cosmic::Theme) -> iced_text::Style {
    let mut color = Color::from(theme.cosmic().on_bg_color());
    color.a = 0.6;
    iced_text::Style { color: Some(color), ..Default::default() }
}

/// The system language's name, for "System (…)".
fn system_language_name() -> &'static str {
    let values = ["LC_ALL", "LC_MESSAGES", "LANG"].map(|k| std::env::var(k).unwrap_or_default());
    let code = maprisma_voice::i18n::system_language([&values[0], &values[1], &values[2]]);
    LANGUAGES.iter().find(|(c, _)| *c == code).map_or("English", |(_, n)| *n)
}

fn spawn(command: Command) {
    if let Err(e) = command.spawn() {
        eprintln!("maprisma-dictation-applet: could not start {}: {e}", command.program.display());
    }
}

impl Applet {
    /// The overlay's settings and shared maprisma-voice settings (Maprisma Settings' Voice page writes them).
    fn reload(&mut self) {
        let s = settings::read();
        maprisma_voice::i18n::set_language(&s.language);
        corners::set_current(&s.corners);
        self.corners = s.corners;
        self.skin = s.skin;
        self.language = s.language;
        self.screenshot_key = s.screenshot_key;
        let voice = maprisma_voice::settings::load(&maprisma_voice::paths::Dirs::from_env());
        self.voice_language = voice.language;
        self.noise_filter = voice.noise_filter;
        self.vad_sensitivity = voice.vad_sensitivity;
        self.hallucination_filter = voice.hallucination_filter;
    }

    /// Saves one setting to the overlay's settings file, then reads them all again.
    fn save(&mut self, key: &str, value: serde_json::Value) {
        self.save_opt(key, Some(value));
    }

    /// Saves (`Some`) or removes (`None`) one setting in the overlay's settings file, then reads them all again.
    fn save_opt(&mut self, key: &str, value: Option<serde_json::Value>) {
        self.setting_error = settings::set_opt(key, value).is_err();
        self.reload();
    }

    /// Saves one shared setting to `~/.config/maprisma-voice/settings.json`, then reloads.
    fn save_voice(&mut self, key: &str, value: serde_json::Value) {
        let dirs = maprisma_voice::paths::Dirs::from_env();
        self.setting_error = maprisma_voice::settings::set(&dirs, key, Some(value)).is_err();
        self.reload();
    }

    fn close_settings_menus(&mut self) {
        self.language_open = false;
        self.corners_open = false;
        self.skin_open = false;
        self.voice_language_open = false;
        self.noise_filter_open = false;
        self.vad_sensitivity_open = false;
    }

    fn any_menu_open(&self) -> bool {
        self.language_open
            || self.corners_open
            || self.skin_open
            || self.voice_language_open
            || self.noise_filter_open
            || self.vad_sensitivity_open
    }

    fn close_popup(&mut self) -> Task<Message> {
        self.settings_open = false;
        self.close_settings_menus();
        match self.popup.take() {
            Some(id) => destroy_popup(id),
            None => Task::none(),
        }
    }

    /// Settings (as Maprisma Launcher's page): ← Back and the title, then one row per setting,
    /// words at the left and the control at the right; a list opens under its row.
    fn settings_page(&self) -> Element<'_, Message> {
        let header = row![
            button::text(tr("Back")).leading_icon(icon::from_name("go-previous-symbolic").size(16)).on_press(Message::ToggleSettings),
            space::horizontal().width(Length::Fill),
            text::caption(tr("Settings").to_uppercase()).class(theme::Text::Custom(dim_text)),
        ]
        .align_y(Alignment::Center);
        let choice = |label: String, open: bool, message: Message| -> Element<'static, Message> {
            button::text(label)
                .trailing_icon(icon::from_name(if open { "pan-up-symbolic" } else { "pan-down-symbolic" }).size(16))
                .on_press(message)
                .into()
        };
        let setting = |label: String, control: Element<'static, Message>| {
            padded_control(row![text::body(label), space::horizontal().width(Length::Fill), control].spacing(12).align_y(Alignment::Center))
        };
        let languages: Vec<(String, String)> = std::iter::once(("system".to_string(), trf("System ({0})", &[system_language_name()])))
            .chain(LANGUAGES.iter().map(|(c, n)| (c.to_string(), n.to_string())))
            .collect();
        let current = languages.iter().find(|(c, _)| *c == self.language).map(|(_, l)| l.clone()).unwrap_or_default();
        let mut page = column![padded_control(header), padded_control(divider::horizontal::default())];
        page = page.push(setting(tr("Language"), choice(current, self.language_open, Message::ToggleLanguage)));
        if self.language_open {
            for (code, label) in languages {
                let label = if code == self.language { format!("✓ {label}") } else { label };
                page = page.push(menu_button(text::body(label)).on_press(Message::Language(code)));
            }
        }
        page = page.push(setting(tr("F12 saves a screenshot"), cosmic::widget::toggler(self.screenshot_key).on_toggle(Message::SetShotKey).into()));
        let desktop = corners::desktop_radius().round().to_string();
        let corner_choices = [
            ("desktop", trf("Like the desktop ({0} px)", &[&desktop])),
            ("slight", trf("Slightly rounded ({0} px)", &["6"])),
            ("rounded", trf("Rounded ({0} px)", &["12"])),
            ("square", tr("Square")),
        ];
        let current = corner_choices.iter().find(|(c, _)| *c == self.corners).map(|(_, l)| l.clone()).unwrap_or_default();
        page = page.push(setting(tr("Corners"), choice(current, self.corners_open, Message::ToggleCorners)));
        if self.corners_open {
            for (choice, label) in corner_choices {
                let label = if self.corners == choice { format!("✓ {label}") } else { label };
                page = page.push(menu_button(text::body(label)).on_press(Message::Corners(choice.into())));
            }
        }
        page = page.push(padded_control(
            row![
                column![
                    text::body(tr("App skin")),
                    text::caption(tr("Visual accent for cards and borders; Native keeps your desktop theme untouched."))
                        .class(theme::Text::Custom(dim_text)),
                ]
                .spacing(2)
                .width(Length::Fill),
                choice(skin::skin_label(&self.skin), self.skin_open, Message::ToggleSkin),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        ));
        if self.skin_open {
            for sk in skin::SKINS {
                let name = skin::skin_label(sk);
                let label = if self.skin == sk { format!("✓ {name}") } else { name };
                let row_inner: Element<Message> = if let Some(st) = skin::skin_style(sk) {
                    row![skin::swatch_dot(st.accent, 10.0), text::body(label)]
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .into()
                } else {
                    text::body(label).into()
                };
                page = page.push(menu_button(row_inner).on_press(Message::SetSkin(sk.into())));
            }
        }
        // Shared voice service settings (same settings.json as Maprisma Settings → Voice).
        page = page.push(padded_control(divider::horizontal::default()));
        page = page.push(padded_control(text::caption(tr("Audio & recognition").to_uppercase()).class(theme::Text::Custom(dim_text))));
        let voice_languages: Vec<(String, String)> = [
            ("auto", tr("Automatic")),
            ("de", "Deutsch".into()),
            ("en", "English".into()),
            ("fr", "Français".into()),
            ("es", "Español".into()),
            ("it", "Italiano".into()),
            ("pt", "Português".into()),
            ("nl", "Nederlands".into()),
            ("pl", "Polski".into()),
        ]
        .into_iter()
        .map(|(c, l)| (c.to_string(), l))
        .collect();
        let current_voice_lang = voice_languages
            .iter()
            .find(|(c, _)| *c == self.voice_language)
            .map(|(_, l)| l.clone())
            .unwrap_or_else(|| self.voice_language.clone());
        page = page.push(setting(tr("Dictation language"), choice(current_voice_lang, self.voice_language_open, Message::ToggleVoiceLanguage)));
        if self.voice_language_open {
            for (code, label) in voice_languages {
                let label = if code == self.voice_language { format!("✓ {label}") } else { label };
                page = page.push(menu_button(text::body(label)).on_press(Message::SetVoiceLanguage(code)));
            }
        }
        let noise_choices = [("off", tr("Off")), ("standard", tr("Standard")), ("strong", tr("Strong"))];
        let current_noise = noise_choices.iter().find(|(c, _)| *c == self.noise_filter).map(|(_, l)| l.clone()).unwrap_or_else(|| tr("Standard"));
        page = page.push(setting(tr("Noise filter"), choice(current_noise, self.noise_filter_open, Message::ToggleNoiseFilter)));
        if self.noise_filter_open {
            for (code, label) in noise_choices {
                let label = if code == self.noise_filter { format!("✓ {label}") } else { label };
                page = page.push(menu_button(text::body(label)).on_press(Message::SetNoiseFilter(code.into())));
            }
        }
        let vad_choices = [("strict", tr("Strict")), ("normal", tr("Normal")), ("sensitive", tr("Sensitive"))];
        let current_vad = vad_choices.iter().find(|(c, _)| *c == self.vad_sensitivity).map(|(_, l)| l.clone()).unwrap_or_else(|| tr("Normal"));
        page = page.push(setting(tr("Voice sensitivity"), choice(current_vad, self.vad_sensitivity_open, Message::ToggleVadSensitivity)));
        if self.vad_sensitivity_open {
            for (code, label) in vad_choices {
                let label = if code == self.vad_sensitivity { format!("✓ {label}") } else { label };
                page = page.push(menu_button(text::body(label)).on_press(Message::SetVadSensitivity(code.into())));
            }
        }
        page = page.push(setting(
            tr("Reject hallucinations"),
            cosmic::widget::toggler(self.hallucination_filter).on_toggle(Message::SetHallucinationFilter).into(),
        ));
        page = page.push(padded_control(divider::horizontal::default()));
        page = page.push(menu_button(text::body(tr("All Voice settings…"))).on_press(Message::Settings));
        if self.setting_error {
            page = page.push(padded_control(text::body(tr("Could not save the setting."))));
        }
        scrollable(page.padding([8, 0]).width(Length::Fixed(POPUP_WIDTH))).into()
    }

    fn popup_content(&self) -> Element<'_, Message> {
        if self.settings_open {
            return self.settings_page();
        }
        let header = row![
            icon::from_name(self.shown.icon()).size(24).symbolic(true),
            column![text::heading(tr("Maprisma Dictation")), text::caption(self.shown.status())].spacing(2),
            space::horizontal().width(Length::Fill),
            // Settings at the header's right end, as in Maprisma Launcher.
            cosmic::widget::tooltip(
                button::icon(icon::from_name("emblem-system-symbolic").symbolic(true)).on_press(Message::ToggleSettings),
                text::body(tr("Settings")),
                cosmic::widget::tooltip::Position::Bottom,
            ),
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        column![
            padded_control(header),
            padded_control(divider::horizontal::default()),
            menu_button(text::body(self.shown.toggle_label())).on_press(Message::MenuToggle),
            menu_button(text::body(tr("Voice settings…"))).on_press(Message::Settings),
        ]
        .padding([8, 0])
        .width(Length::Fixed(POPUP_WIDTH))
        .into()
    }
}

impl cosmic::Application for Applet {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = bar::APP_ID;

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: ()) -> (Self, Task<Message>) {
        let mut app = Self { core, ..Default::default() };
        app.reload();
        // Debug builds only: start on the settings page (for off-screen screenshots).
        app.settings_open = cfg!(debug_assertions) && std::env::var("MAPRISMA_SHOT_PAGE").as_deref() == Ok("settings");
        look::reload();
        (app, Task::none())
    }

    fn system_theme_update(&mut self, _keys: &[&'static str], new_theme: &cosmic::cosmic_theme::Theme) -> Task<Message> {
        let _ = corners::system_theme_update::<Message>(&self.corners, new_theme, false);
        Task::none()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::Closed(id))
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            watch(),
            cosmic::iced::time::every(Duration::from_secs(5)).map(|_| Message::Tick),
            rectangle_tracker_subscription(0).map(|update| Message::Rectangle(update.1)),
            // Esc: an open list, then Back from the settings page, then the menu closes (layout rule 5a).
            cosmic::iced::event::listen_with(|event, status, id| match event {
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed { key: cosmic::iced::keyboard::Key::Named(cosmic::iced::keyboard::key::Named::Escape), .. })
                    if status == cosmic::iced::event::Status::Ignored =>
                {
                    Some(Message::Escape(id))
                }
                _ => None,
            }),
        ])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Toggle => {
                spawn(Command::toggle());
                return self.close_popup();
            }
            Message::MenuToggle => {
                spawn(Command::toggle());
                return self.close_popup();
            }
            Message::Settings => {
                spawn(bar::settings_command());
                return self.close_popup();
            }
            Message::Shown(shown) => self.shown = shown,
            Message::Tick => {
                if !self.any_menu_open() {
                    self.reload();
                }
                look::refresh();
            }
            Message::Escape(id) => match esc_step(self.any_menu_open(), self.settings_open, self.popup == Some(id)) {
                EscStep::Menus => self.close_settings_menus(),
                EscStep::Back => self.settings_open = false,
                EscStep::Close => return self.close_popup(),
                EscStep::Nothing => {}
            },
            Message::ToggleSettings => {
                self.settings_open = !self.settings_open;
                self.close_settings_menus();
                if self.settings_open {
                    self.reload();
                }
            }
            Message::ToggleLanguage => {
                let next = !self.language_open;
                self.close_settings_menus();
                self.language_open = next;
            }
            Message::ToggleCorners => {
                let next = !self.corners_open;
                self.close_settings_menus();
                self.corners_open = next;
            }
            Message::ToggleSkin => {
                let next = !self.skin_open;
                self.close_settings_menus();
                self.skin_open = next;
            }
            Message::ToggleVoiceLanguage => {
                let next = !self.voice_language_open;
                self.close_settings_menus();
                self.voice_language_open = next;
            }
            Message::ToggleNoiseFilter => {
                let next = !self.noise_filter_open;
                self.close_settings_menus();
                self.noise_filter_open = next;
            }
            Message::ToggleVadSensitivity => {
                let next = !self.vad_sensitivity_open;
                self.close_settings_menus();
                self.vad_sensitivity_open = next;
            }
            Message::Language(code) => {
                self.language_open = false;
                self.save("language", code.into());
            }
            Message::Corners(choice) => {
                self.corners_open = false;
                self.save("corners", choice.into());
            }
            Message::SetSkin(choice) => {
                let choice = skin::normalize_skin(&choice);
                let value = (choice != "native").then(|| serde_json::Value::from(choice));
                self.skin_open = false;
                self.save_opt(skin::SKIN_KEY, value);
            }
            Message::SetShotKey(on) => self.save("screenshot_key", on.into()),
            Message::SetVoiceLanguage(code) => {
                self.voice_language_open = false;
                self.save_voice("language", code.into());
            }
            Message::SetNoiseFilter(mode) => {
                self.noise_filter_open = false;
                self.save_voice("noise_filter", mode.into());
            }
            Message::SetVadSensitivity(level) => {
                self.vad_sensitivity_open = false;
                self.save_voice("vad_sensitivity", level.into());
            }
            Message::SetHallucinationFilter(on) => self.save_voice("hallucination_filter", on.into()),
            Message::Closed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                    self.settings_open = false;
                    self.close_settings_menus();
                }
            }
            Message::Rectangle(update) => match update {
                RectangleUpdate::Rectangle(r) => self.rectangle = r.1,
                RectangleUpdate::Init(tracker) => self.tracker = Some(tracker),
            },
            Message::TogglePopup => {
                if self.popup.is_some() {
                    return self.close_popup();
                }
                self.reload();
                return cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                    |_| Default::default(),
                    |app: &mut Self| {
                        let id = window::Id::unique();
                        app.popup = Some(id);
                        let mut settings = app.core.applet.get_popup_settings(app.core.main_window_id().unwrap(), id, None, None, None);
                        let Rectangle { x, y, width, height } = app.rectangle;
                        settings.positioner.anchor_rect =
                            Rectangle::<i32> { x: x.max(1.0) as i32, y: y.max(1.0) as i32, width: width.max(1.0) as i32, height: height.max(1.0) as i32 };
                        settings.positioner.size = None;
                        settings
                    },
                    None,
                ));
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        look::keep_fonts();
        let horizontal = matches!(self.core.applet.anchor, PanelAnchor::Top | PanelAnchor::Bottom);
        let padding = self.core.applet.suggested_padding(true).0;
        let size = self.core.applet.suggested_size(true).0;
        let mut bar_icon = icon::from_name(self.shown.icon()).size(size).symbolic(true).icon();
        if self.shown.active() {
            // Accent while the microphone listens, as every Maprisma bar icon that is on.
            let custom = skin::skin_style(&self.skin).map(|st| st.accent);
            bar_icon = bar_icon.class(theme::Svg::custom(move |theme: &cosmic::Theme| iced_svg::Style {
                color: Some(custom.unwrap_or_else(|| theme.cosmic().accent_color().into())),
            }));
        }
        let button = button::custom(bar_icon)
            .padding(if horizontal { [0, padding] } else { [padding, 0] })
            .on_press_down(Message::Toggle)
            .class(theme::Button::AppletIcon);
        let button = mouse_area(button).on_right_press(Message::TogglePopup);
        let content: Element<Message> = match self.tracker.as_ref() {
            Some(tracker) => tracker.container(0, button).ignore_bounds(true).into(),
            None => button.into(),
        };
        autosize::autosize(content, AUTOSIZE_ID.clone()).into()
    }

    fn view_window(&self, _id: window::Id) -> Element<'_, Message> {
        look::keep_fonts();
        skin::apply_popup(&self.skin, &self.corners, &self.core, self.popup_content())
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_closes_the_innermost_thing_first() {
        assert_eq!(esc_step(true, true, true), EscStep::Menus);
        assert_eq!(esc_step(false, true, true), EscStep::Back);
        assert_eq!(esc_step(false, false, true), EscStep::Close);
        // Not from the open popup (e.g. the panel button): nothing.
        assert_eq!(esc_step(true, true, false), EscStep::Nothing);
        assert_eq!(esc_step(false, false, false), EscStep::Nothing);
    }
}
