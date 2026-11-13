//! maprisma-dictation-applet: Maprisma Dictation's icon in the COSMIC panel. A click opens the
//! overlay (it listens at once) or closes it: `maprisma-dictation --toggle`, the overlay's own
//! toggle. A right click opens a small menu: open/close dictation, Voice settings… (Maprisma
//! Manager's Voice page). The icon follows the overlay and the voice service (src/bar.rs).

use maprisma_dictation::bar::{self, Command, Shown};
use maprisma_dictation::{corners, look, settings};
use maprisma_voice::i18n::tr;
use std::sync::LazyLock;
use std::time::Duration;

use cosmic::app::{Core, Task};
use cosmic::applet::cosmic_panel_config::PanelAnchor;
use cosmic::applet::{menu_button, padded_control};
use cosmic::iced::platform_specific::shell::wayland::commands::popup::destroy_popup;
use cosmic::iced::{Alignment, Length, Rectangle, Subscription, window};
use cosmic::widget::rectangle_tracker::{RectangleTracker, RectangleUpdate, rectangle_tracker_subscription};
use cosmic::widget::{Id, autosize, button, column, divider, icon, mouse_area, row, text};
use cosmic::{Element, theme};

static AUTOSIZE_ID: LazyLock<Id> = LazyLock::new(|| Id::new("dictation-autosize"));
const POPUP_WIDTH: f32 = 300.0;

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<Applet>(())
}

#[derive(Default)]
struct Applet {
    core: Core,
    shown: Shown,
    corners: String,
    popup: Option<window::Id>,
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

fn spawn(command: Command) {
    if let Err(e) = command.spawn() {
        eprintln!("maprisma-dictation-applet: could not start {}: {e}", command.program.display());
    }
}

impl Applet {
    /// The overlay's settings: language and corners (Maprisma Manager's Voice page writes them).
    fn reload(&mut self) {
        let s = settings::read();
        maprisma_voice::i18n::set_language(&s.language);
        corners::set_current(&s.corners);
        self.corners = s.corners;
    }

    fn close_popup(&mut self) -> Task<Message> {
        match self.popup.take() {
            Some(id) => destroy_popup(id),
            None => Task::none(),
        }
    }

    fn popup_content(&self) -> Element<'_, Message> {
        let header = row![
            icon::from_name(self.shown.icon()).size(24).symbolic(true),
            column![text::heading(tr("Maprisma Dictation")), text::caption(self.shown.status())].spacing(2),
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
        look::reload();
        (app, Task::none())
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::Closed(id))
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            watch(),
            cosmic::iced::time::every(Duration::from_secs(5)).map(|_| Message::Tick),
            rectangle_tracker_subscription(0).map(|update| Message::Rectangle(update.1)),
            // Esc closes the menu (layout rule 5a; it has nothing inside to close first).
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
                self.reload();
                look::refresh();
            }
            Message::Escape(id) => {
                if self.popup == Some(id) {
                    return self.close_popup();
                }
            }
            Message::Closed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
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
        let horizontal = matches!(self.core.applet.anchor, PanelAnchor::Top | PanelAnchor::Bottom);
        let padding = self.core.applet.suggested_padding(true).0;
        let size = self.core.applet.suggested_size(true).0;
        let button = button::custom(icon::from_name(self.shown.icon()).size(size).symbolic(true).icon())
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
        corners::apply(&self.corners, self.core.applet.popup_container(self.popup_content()))
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}
