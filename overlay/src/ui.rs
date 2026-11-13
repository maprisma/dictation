//! The overlay: a layer-shell surface at the top centre (48 px below the top edge, keyboard on
//! demand) holding the pill, its aura ring and the answer; plus, while closing by a click
//! would lose nothing, a transparent full-screen layer under it that catches clicks outside.
//! The decisions are in state.rs, the ring's maths in aura.rs; this file draws and wires.

use crate::aura::{self, AuraColors, Envelope, Tone};
use crate::helper::{self, Exchange};
use crate::instance::Command;
use crate::markdown::{self, Block};
use crate::settings::{self, Settings};
use crate::state::{Effect, End, Event, Machine, Phase};
use crate::voice::{self, Control, Controls, VoiceEvent};
use crate::{corners, look, shot};
use cosmic::app::{Core, Task};
use cosmic::cctk::sctk::reexports::client::protocol::wl_output::WlOutput;
use cosmic::iced::event::wayland::{Event as WaylandEvent, OutputEvent};
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::{IcedMargin, IcedOutput, SctkLayerSurfaceSettings};
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface, set_input_zone,
};
use cosmic::iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use cosmic::iced::widget::{container as iced_container, text as iced_text};
use cosmic::iced::{Background, Border, Color, Length, Point, Rectangle, Size, Subscription, mouse, window};
use cosmic::widget::{column, container, mouse_area, row, scrollable, space, text};
use cosmic::{Element, theme};
use maprisma_voice::i18n::{tr, trf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

pub const APP_ID: &str = "com.maprisma.Dictation";
// F12 pictures go to ~/Pictures/<this>/ (the app's name, the same in every language).
const SHOT_FOLDER: &str = "Maprisma Dictation";
static INPUT_ID: LazyLock<cosmic::widget::Id> = LazyLock::new(|| cosmic::widget::Id::new("dictation-input"));

/// The pill: compact, and at most this big.
pub const COMPACT: (f32, f32) = (380.0, 52.0);
pub const EXPANDED_WIDTH: f32 = 480.0;
pub const MAX_HEIGHT: f32 = 360.0;
/// Room around the pill for the bloom and the shake.
pub const MARGIN: f32 = 24.0;
/// The pill's top edge sits this far below the top of the screen (or under a panel).
pub const TOP: f32 = 48.0;
const SURFACE: (f32, f32) = (EXPANDED_WIDTH + 2.0 * MARGIN, MAX_HEIGHT + 2.0 * MARGIN);
/// Space for the state dot left of the field.
const LEAD: f32 = 40.0;

/// The helper's process group while it runs (killed when the overlay closes).
static HELPER_GROUP: AtomicI32 = AtomicI32::new(0);

#[derive(Debug, Clone, Default)]
pub struct Flags {
    /// Hold-to-talk: listen until `--release` (or a tap restarts it ending on silence).
    pub hold: bool,
    /// `--demo`: made-up levels, speech and answer; no voice service or helper is used.
    pub demo: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    Output(String, WlOutput),
    OpenFallback,
    Command(Command),
    Voice(u64, VoiceEvent),
    Input(String),
    Submit,
    Escape,
    CopyAnswer,
    ToggleMic,
    ShotKey(window::Id),
    Captured(window::Screenshot),
    ShotSaved(Result<shot::Saved, String>),
    Answer(u64, Result<String, String>, String),
    Frame,
    LookTick,
    ClickOutside,
    ClearStatus(u64),
    /// The answer's measured height.
    BodySize(Size),
}

pub struct Overlay {
    core: Core,
    flags: Flags,
    settings: Settings,
    reduced: bool,
    machine: Machine,
    started: Instant,
    // surfaces
    output_wanted: Option<String>,
    output: Option<IcedOutput>,
    surface: Option<window::Id>,
    catcher: Option<window::Id>,
    zone: Option<Rectangle>,
    // the voice session
    voice_gen: u64,
    voice_on: Option<End>,
    controls: Option<Controls>,
    queued: Vec<Control>,
    // the helper
    ask_gen: u64,
    ask: Option<cosmic::iced::task::Handle>,
    helper_name: String,
    last: Option<Exchange>,
    // animation
    envelope: Envelope,
    target_level: f32,
    last_frame: Instant,
    phase_since: Instant,
    done_at: Option<Instant>,
    error_at: Option<Instant>,
    size_from: (f32, f32),
    size_to: (f32, f32),
    size_since: Instant,
    // the footer's short notes
    status: Option<String>,
    status_gen: u64,
    capturing: bool,
    /// The body's measured height (None until laid out).
    body_height: Option<f32>,
}

fn rgb(c: Color) -> aura::Rgb {
    [c.r, c.g, c.b]
}

fn color(c: aura::Rgb, a: f32) -> Color {
    Color::from_rgba(c[0], c[1], c[2], a)
}

fn off_thread<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static, done: fn(Result<T, String>) -> Message) -> Task<Message> {
    let (sender, receiver) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    Task::perform(receiver, move |result| cosmic::Action::App(done(result.unwrap_or_else(|_| Err("failed".into())))))
}

/// The answer's height in the pill (a rough line count; the body scrolls when it is more).
pub fn estimate_height(blocks: &[Block], question: bool, width: f32) -> f32 {
    let per_line = ((width - 2.0 * 16.0) / 7.4).max(20.0) as usize;
    let lines = |t: &str| t.lines().map(|l| l.chars().count().div_ceil(per_line).max(1)).sum::<usize>() as f32;
    let mut h = COMPACT.1 + 1.0 + 12.0; // the field, a divider, padding
    if question {
        h += 22.0;
    }
    for b in blocks {
        h += 8.0 + match b {
            Block::Heading(t) => 22.0 * lines(t),
            Block::Paragraph(t) => 20.5 * lines(t),
            Block::Item(_, t) => 20.5 * lines(t),
            Block::Code(t) => 18.0 * t.lines().count().max(1) as f32 + 16.0,
        };
    }
    (h + 12.0 + 26.0).clamp(COMPACT.1, MAX_HEIGHT) // the footer
}

impl Overlay {
    fn now(&self) -> f32 {
        self.started.elapsed().as_secs_f32()
    }

    fn layer_settings(&self, id: window::Id) -> SctkLayerSurfaceSettings {
        SctkLayerSurfaceSettings {
            id,
            layer: Layer::Overlay,
            keyboard_interactivity: KeyboardInteractivity::OnDemand,
            anchor: Anchor::TOP,
            namespace: "maprisma-dictation".into(),
            margin: IcedMargin { top: (TOP - MARGIN) as i32, ..Default::default() },
            size: Some((Some(SURFACE.0 as u32), Some(SURFACE.1 as u32))),
            output: self.output.clone().unwrap_or_default(),
            exclusive_zone: 0,
            ..Default::default()
        }
    }

    fn open(&mut self, output: IcedOutput) -> Task<Message> {
        if self.surface.is_some() {
            return Task::none();
        }
        self.output = Some(output);
        let id = window::Id::unique();
        self.surface = Some(id);
        let listen = if !self.settings.listen && !self.flags.hold {
            None
        } else if self.flags.hold {
            Some(End::Release)
        } else if self.settings.end_silence {
            Some(End::Silence)
        } else {
            Some(End::Release)
        };
        let effects = self.machine.handle(Event::Opened { listen });
        let zone = self.update_zone(true);
        Task::batch([get_layer_surface(self.layer_settings(id)), zone, cosmic::widget::text_input::focus(INPUT_ID.clone()), self.run(effects)])
    }

    /// The pill's size for what shows now.
    fn target_size(&self) -> (f32, f32) {
        let m = &self.machine;
        let expanded = !m.answer.is_empty() || matches!(m.phase, Phase::Thinking | Phase::Error(_)) || m.note.is_some();
        if !expanded {
            return COMPACT;
        }
        let height = match &m.phase {
            Phase::Thinking => COMPACT.1 + 1.0 + 10.0 + 22.0 + 12.0,
            Phase::Error(e) => COMPACT.1 + 1.0 + 12.0 + 22.0 * (e.chars().count() as f32 / 58.0).ceil().max(1.0) + 12.0 + 26.0,
            _ if !m.answer.is_empty() => match self.body_height {
                // the field, divider, gap, body, footer
                Some(body) => COMPACT.1 + 1.0 + 10.0 + body + 4.0 + 30.0,
                None => estimate_height(&markdown::parse(&m.answer), !m.question.is_empty(), EXPANDED_WIDTH),
            },
            _ => COMPACT.1 + 30.0,
        };
        (EXPANDED_WIDTH, height.min(MAX_HEIGHT))
    }

    /// The pill's size at this moment (220 ms morph).
    fn size_now(&self) -> (f32, f32) {
        let t = self.size_since.elapsed().as_secs_f32();
        let p = if self.reduced { 1.0 } else { aura::morph(t) };
        let lerp = |a: f32, b: f32| a + (b - a) * p;
        (lerp(self.size_from.0, self.size_to.0), lerp(self.size_from.1, self.size_to.1))
    }

    /// Starts a morph when the content's size changed; the input zone follows.
    fn update_zone(&mut self, force: bool) -> Task<Message> {
        let target = self.target_size();
        if target != self.size_to {
            self.size_from = self.size_now();
            self.size_to = target;
            self.size_since = Instant::now();
        } else if !force {
            return Task::none();
        }
        let (w, h) = (self.size_from.0.max(self.size_to.0), self.size_from.1.max(self.size_to.1));
        let zone = Rectangle::new(Point::new((SURFACE.0 - w) / 2.0 - 6.0, MARGIN - 6.0), Size::new(w + 12.0, h + 12.0));
        if self.zone == Some(zone) {
            return Task::none();
        }
        self.zone = Some(zone);
        match self.surface {
            Some(id) => set_input_zone(id, Some(vec![zone])),
            None => Task::none(),
        }
    }

    /// The catcher layer exists exactly while a click outside would close.
    fn sync_catcher(&mut self) -> Task<Message> {
        let want = self.surface.is_some() && self.machine.click_closes();
        match (want, self.catcher) {
            (true, None) => {
                let id = window::Id::unique();
                self.catcher = Some(id);
                get_layer_surface(SctkLayerSurfaceSettings {
                    id,
                    layer: Layer::Top,
                    keyboard_interactivity: KeyboardInteractivity::None,
                    anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                    namespace: "maprisma-dictation-outside".into(),
                    size: Some((None, None)),
                    exclusive_zone: -1,
                    output: self.output.clone().unwrap_or_default(),
                    ..Default::default()
                })
            }
            (false, Some(id)) => {
                self.catcher = None;
                destroy_layer_surface(id)
            }
            _ => Task::none(),
        }
    }

    fn handle(&mut self, event: Event) -> Task<Message> {
        let before = self.machine.phase.clone();
        let effects = self.machine.handle(event);
        if self.machine.phase != before {
            self.phase_since = Instant::now();
        }
        self.run(effects)
    }

    fn send_control(&mut self, control: Control) {
        match &self.controls {
            Some(c) => {
                let _ = c.0.send(control);
            }
            None => self.queued.push(control),
        }
    }

    fn run(&mut self, effects: Vec<Effect>) -> Task<Message> {
        let mut tasks = vec![];
        for effect in effects {
            match effect {
                Effect::StartVoice(end) => {
                    self.voice_gen += 1;
                    self.voice_on = Some(end);
                    self.controls = None;
                    self.queued.clear();
                    self.envelope = Envelope::default();
                    self.target_level = 0.0;
                }
                Effect::FinishVoice => self.send_control(Control::Finish),
                Effect::CancelVoice => {
                    if let Some(c) = self.controls.take() {
                        let _ = c.0.send(Control::Cancel);
                    }
                    // Dropping the subscription closes its connection: the service cancels.
                    self.voice_on = None;
                    self.queued.clear();
                    self.target_level = 0.0;
                }
                Effect::Ask(question) => {
                    self.body_height = None;
                    tasks.push(self.ask(question));
                }
                Effect::CancelAsk => self.cancel_ask(),
                Effect::FlashDone => self.done_at = Some(Instant::now()),
                Effect::FlashError => self.error_at = Some(Instant::now()),
                Effect::Close => {
                    self.cancel_ask();
                    if let Some(c) = self.controls.take() {
                        let _ = c.0.send(Control::Cancel);
                    }
                    let mut close = vec![];
                    if let Some(id) = self.catcher.take() {
                        close.push(destroy_layer_surface(id));
                    }
                    if let Some(id) = self.surface.take() {
                        close.push(destroy_layer_surface(id));
                    }
                    // Give the Cancel a moment to reach the service, then leave.
                    close.push(Task::perform(tokio::time::sleep(Duration::from_millis(60)), |_| cosmic::Action::App(Message::Frame)).chain(cosmic::iced::exit()));
                    return Task::batch(close);
                }
            }
        }
        tasks.push(self.update_zone(false));
        tasks.push(self.sync_catcher());
        Task::batch(tasks)
    }

    fn cancel_ask(&mut self) {
        self.ask_gen += 1;
        if let Some(handle) = self.ask.take() {
            handle.abort();
        }
        kill_helper();
    }

    fn ask(&mut self, question: String) -> Task<Message> {
        self.cancel_ask();
        let generation = self.ask_gen;
        let context = helper::follow_up(self.last.as_ref(), crate::last::now()).cloned();
        let language = ui_language(&self.settings.language);
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".into());
        let demo = self.flags.demo;
        let work = async move {
            if demo {
                tokio::time::sleep(Duration::from_millis(1600)).await;
                return (Ok(voice::DEMO_ANSWER.to_string()), "Demo".to_string());
            }
            let found = tokio::task::spawn_blocking(|| {
                let installed = helper::installed();
                helper::choose(&crate::shared_ai::choice(), &installed)
            })
            .await
            .ok()
            .flatten();
            let Some(chosen) = found else {
                return (Err(tr("No AI helper is installed. Choose one in Maprisma Manager → AI.")), String::new());
            };
            let name = chosen.installed.helper.name.clone();
            let prompt = helper::build_prompt(&question, context.as_ref(), &language, &desktop);
            let stdin = chosen.installed.helper.stdin.then(|| prompt.clone());
            let argv = helper::ask_argv(&chosen, &prompt);
            let work = match crate::paths::runtime_dir() {
                Ok(dir) => {
                    let work = dir.join("work");
                    let _ = std::fs::create_dir(&work);
                    work
                }
                Err(e) => return (Err(e.to_string()), name),
            };
            (run_helper(argv, stdin, work).await, name)
        };
        let (task, handle) = Task::perform(work, move |(result, name)| cosmic::Action::App(Message::Answer(generation, result, name))).abortable();
        self.ask = Some(handle);
        task
    }

    fn set_status(&mut self, text: String) -> Task<Message> {
        self.status = Some(text);
        self.status_gen += 1;
        let generation = self.status_gen;
        Task::perform(tokio::time::sleep(Duration::from_millis(2200)), move |_| cosmic::Action::App(Message::ClearStatus(generation)))
    }

    fn animating(&self) -> bool {
        if self.surface.is_none() {
            return false;
        }
        let flashing = self.done_at.is_some_and(|t| t.elapsed().as_secs_f32() < aura::DONE_FLASH)
            || self.error_at.is_some_and(|t| t.elapsed().as_secs_f32() < 0.6)
            || self.size_since.elapsed().as_secs_f32() < aura::MORPH;
        if self.reduced {
            return flashing || self.voice_on.is_some() || self.envelope.value > 0.01;
        }
        flashing || matches!(self.machine.phase, Phase::Listening | Phase::Transcribing | Phase::Thinking) || self.envelope.value > 0.01
    }

    // ----------------------------------------------------------------------------- drawing

    fn aura(&self) -> AuraCanvas {
        let theme = cosmic::theme::active();
        let c = theme.cosmic();
        let dark = c.is_dark;
        let accent = rgb(c.accent_color().into());
        let t = self.now();
        let phase_t = self.phase_since.elapsed().as_secs_f32();
        let done = self.done_at.map(|d| aura::done_flash(d.elapsed().as_secs_f32())).unwrap_or(0.0);
        let error_t = self.error_at.map(|e| e.elapsed().as_secs_f32());
        let (tone, mode) = match &self.machine.phase {
            Phase::Listening => (Tone::Accent, Mode::Listening),
            Phase::Transcribing => (Tone::Accent, Mode::Transcribing),
            Phase::Thinking => (Tone::Accent, Mode::Thinking),
            Phase::Error(_) => (Tone::Error, Mode::Error),
            Phase::Done if done > 0.0 => (Tone::Done, Mode::Done),
            _ => (Tone::Accent, Mode::Idle),
        };
        let (w, h) = self.size_now();
        let shake = match error_t {
            Some(e) if !self.reduced => aura::shake(e),
            _ => 0.0,
        };
        let sizes = corners::sizes(&self.settings.corners);
        let radius = if corners::normalize(&self.settings.corners) == "square" { 0.0 } else { sizes.radius.clamp(12.0, 26.0) };
        let mut surface: Color = c.bg_color().into();
        surface.a = 0.92;
        AuraCanvas {
            pill: Rectangle::new(Point::new((SURFACE.0 - w) / 2.0 + shake, MARGIN), Size::new(w, h)),
            radius: radius.min(h / 2.0),
            colors: aura::aura_colors(accent, dark, tone),
            accent: aura::aura_colors(accent, dark, Tone::Accent),
            surface,
            mode,
            level: self.envelope.value,
            t,
            phase_t,
            done,
            error_t: error_t.unwrap_or(10.0),
            reduced: self.reduced,
        }
    }

    fn field(&self) -> Element<'_, Message> {
        let m = &self.machine;
        let listening = self.voice_on.is_some();
        let placeholder = if listening && !m.partial.is_empty() {
            String::new()
        } else {
            match m.phase {
                Phase::Listening => tr("Listening…"),
                Phase::Transcribing => tr("Transcribing…"),
                Phase::Thinking => tr("Thinking…"),
                _ if !m.answer.is_empty() => tr("Ask a follow-up…"),
                _ => tr("Speak or type…"),
            }
        };
        let input = cosmic::widget::text_input::inline_input(placeholder, m.text.as_str())
            .id(INPUT_ID.clone())
            .on_input(Message::Input)
            .on_submit(|_| Message::Submit)
            .size(15.0)
            .width(Length::Fill);
        // The voice service's text so far, at 60 %, over the (empty) field.
        let field: Element<Message> = if listening && !m.partial.is_empty() {
            let partial = text::body(tail(&m.partial, 52)).size(15.0).class(theme::Text::Custom(partial_text));
            cosmic::iced::widget::stack![input, container(partial).padding([0, 8]).height(Length::Fill).align_y(cosmic::iced::Alignment::Center)].into()
        } else {
            input.into()
        };
        let hint = match m.phase {
            Phase::Listening | Phase::Transcribing | Phase::Thinking => "Esc",
            _ if !m.text.is_empty() => "Enter",
            _ => "Esc",
        };
        row![
            space::horizontal().width(Length::Fixed(LEAD - 8.0)),
            field,
            container(text::caption(hint).class(theme::Text::Custom(dim_text))).padding([2, 6]).class(theme::Container::custom(key_badge)),
        ]
        .spacing(8)
        .padding([0, 12, 0, 0])
        .height(Length::Fixed(COMPACT.1))
        .align_y(cosmic::iced::Alignment::Center)
        .into()
    }

    fn body(&self) -> Option<Element<'_, Message>> {
        let m = &self.machine;
        let mut col = cosmic::iced::widget::Column::new().spacing(8).width(Length::Fill);
        let mut any = false;
        if !m.question.is_empty() && (m.phase == Phase::Thinking || !m.answer.is_empty() || matches!(m.phase, Phase::Error(_))) {
            col = col.push(text::caption(tail(&m.question, 400)).class(theme::Text::Custom(dim_text)));
            any = true;
        }
        match &m.phase {
            Phase::Error(e) => {
                col = col.push(text::body(if e.is_empty() { tr("Something went wrong.") } else { tr(e) }).class(theme::Text::Custom(error_text)));
                any = true;
            }
            _ => {}
        }
        if !m.answer.is_empty() && m.phase != Phase::Thinking {
            for block in markdown::parse(&m.answer) {
                col = col.push(block_view(block));
            }
            any = true;
        }
        if let Some(note) = &m.note {
            col = col.push(text::caption(tr(note)).class(theme::Text::Custom(dim_text)));
            any = true;
        }
        let measured = cosmic::iced::widget::sensor(container(col).padding([0, 20, 4, 16]).width(Length::Fixed(self.size_to.0)))
            .on_show(Message::BodySize)
            .on_resize(Message::BodySize);
        any.then(|| scrollable(measured).height(Length::Fill).into())
    }

    fn footer(&self) -> Element<'_, Message> {
        let left = self.status.clone().unwrap_or_else(|| self.helper_name.clone());
        let hints = if self.machine.answer.is_empty() { tr("Enter sends · Ctrl+M talks · Esc closes") } else { tr("Enter asks · Ctrl+Shift+C copies · Esc closes") };
        row![
            text::caption(left).class(theme::Text::Custom(dim_text)),
            space::horizontal().width(Length::Fill),
            text::caption(hints).class(theme::Text::Custom(dim_text)),
        ]
        .padding([4, 16, 8, 16])
        .into()
    }

    fn pill(&self) -> Element<'_, Message> {
        let (w, h) = self.size_now();
        let shake = match self.error_at {
            Some(e) if !self.reduced => aura::shake(e.elapsed().as_secs_f32()),
            _ => 0.0,
        };
        let mut content = cosmic::iced::widget::Column::new().push(self.field());
        let expanded = h > COMPACT.1 + 8.0;
        if expanded {
            if let Some(body) = self.body() {
                content = content.push(container(cosmic::widget::divider::horizontal::light()).padding([0, 12])).push(space::vertical().height(Length::Fixed(10.0))).push(body);
                if !self.machine.answer.is_empty() || matches!(self.machine.phase, Phase::Error(_)) {
                    content = content.push(self.footer());
                }
            }
        }
        let pill = container(content).width(Length::Fixed(w)).height(Length::Fixed(h)).clip(true);
        let left = ((SURFACE.0 - w) / 2.0 + shake).max(0.0);
        let placed = column![space::vertical().height(Length::Fixed(MARGIN)), row![space::horizontal().width(Length::Fixed(left)), pill]];
        let ring = cosmic::iced::widget::canvas(self.aura()).width(Length::Fill).height(Length::Fill);
        let stack: Element<Message> = cosmic::iced::widget::stack![ring, placed].into();
        mouse_area(container(stack).width(Length::Fixed(SURFACE.0)).height(Length::Fixed(SURFACE.1))).into()
    }
}

/// The last `max` characters (live speech scrolls left in the compact field).
fn tail(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max { text.to_string() } else { format!("…{}", text.chars().skip(n - max + 1).collect::<String>()) }
}

fn block_view(block: Block) -> Element<'static, Message> {
    match block {
        Block::Heading(t) => text::heading(t).into(),
        Block::Paragraph(t) => text::body(t).size(14.5).line_height(cosmic::iced::widget::text::LineHeight::Relative(1.45)).into(),
        Block::Item(marker, t) => row![
            text::body(marker).width(Length::Fixed(22.0)),
            text::body(t).size(14.5).line_height(cosmic::iced::widget::text::LineHeight::Relative(1.45)).width(Length::Fill),
        ]
        .into(),
        Block::Code(t) => container(text::body(t).font(cosmic::font::mono()).size(12.5)).padding(8).width(Length::Fill).class(theme::Container::custom(code_box)).into(),
    }
}

fn dim_text(theme: &cosmic::Theme) -> iced_text::Style {
    let mut color = Color::from(theme.cosmic().on_bg_color());
    color.a = 0.65;
    iced_text::Style { color: Some(color), ..Default::default() }
}

fn partial_text(theme: &cosmic::Theme) -> iced_text::Style {
    let mut color = Color::from(theme.cosmic().on_bg_color());
    color.a = 0.6;
    iced_text::Style { color: Some(color), ..Default::default() }
}

fn error_text(theme: &cosmic::Theme) -> iced_text::Style {
    iced_text::Style { color: Some(theme.cosmic().destructive_color().into()), ..Default::default() }
}

fn key_badge(theme: &cosmic::Theme) -> iced_container::Style {
    let mut border = Color::from(theme.cosmic().on_bg_color());
    border.a = 0.18;
    iced_container::Style { border: Border { radius: corners::now().small.into(), width: 1.0, color: border }, ..Default::default() }
}

fn code_box(theme: &cosmic::Theme) -> iced_container::Style {
    let mut bg = Color::from(theme.cosmic().on_bg_color());
    bg.a = 0.07;
    iced_container::Style { background: Some(Background::Color(bg)), border: Border { radius: corners::now().control.into(), ..Default::default() }, ..Default::default() }
}

/// The overlay's language code (the setting, or the session's for "system").
fn ui_language(setting: &str) -> String {
    if setting != "system" {
        return setting.to_string();
    }
    let values = ["LC_ALL", "LC_MESSAGES", "LANG"].map(|k| std::env::var(k).unwrap_or_default());
    maprisma_voice::i18n::system_language([&values[0], &values[1], &values[2]]).to_string()
}

fn kill_helper() {
    let group = HELPER_GROUP.swap(0, Ordering::SeqCst);
    if group > 0 {
        unsafe {
            libc::kill(-group, libc::SIGTERM);
        }
    }
}

/// [helper::run] in its own process group, so a cancel stops everything it started.
async fn run_helper(argv: Vec<String>, stdin: Option<String>, work: std::path::PathBuf) -> Result<String, String> {
    struct Group;
    impl Drop for Group {
        fn drop(&mut self) {
            kill_helper();
        }
    }
    let _group = Group;
    helper::run_with(argv, stdin, work, helper::TIMEOUT, |pid| HELPER_GROUP.store(pid, Ordering::SeqCst)).await
}

// ------------------------------------------------------------------------------ the ring

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    Idle,
    Listening,
    Transcribing,
    Thinking,
    Done,
    Error,
}

struct AuraCanvas {
    pill: Rectangle,
    radius: f32,
    colors: AuraColors,
    accent: AuraColors,
    surface: Color,
    mode: Mode,
    level: f32,
    t: f32,
    phase_t: f32,
    done: f32,
    error_t: f32,
    reduced: bool,
}

fn rounded(r: Rectangle, radius: f32) -> Path {
    Path::rounded_rectangle(Point::new(r.x, r.y), Size::new(r.width.max(0.0), r.height.max(0.0)), radius.max(0.0).into())
}

fn grow(r: Rectangle, by: f32) -> Rectangle {
    Rectangle::new(Point::new(r.x - by, r.y - by), Size::new(r.width + 2.0 * by, r.height + 2.0 * by))
}

impl AuraCanvas {
    /// The stroke's gradient: across the pill, turned by `angle`.
    fn gradient(&self, colors: &AuraColors, angle: f32, alpha: f32) -> canvas::Style {
        let c = self.pill.center();
        let (dx, dy) = (angle.cos() * self.pill.width / 2.0, angle.sin() * self.pill.width / 2.0);
        let linear = canvas::gradient::Linear::new(Point::new(c.x - dx, c.y - dy), Point::new(c.x + dx, c.y + dy))
            .add_stop(0.0, color(colors.primary, alpha))
            .add_stop(0.5, color(colors.secondary, alpha))
            .add_stop(1.0, color(colors.peak, alpha));
        canvas::Style::Gradient(canvas::Gradient::Linear(linear))
    }
}

impl<Message> canvas::Program<Message, cosmic::Theme, cosmic::Renderer> for AuraCanvas {
    type State = ();

    fn draw(&self, _: &(), renderer: &cosmic::Renderer, _: &cosmic::Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry<cosmic::Renderer>> {
        let mut frame = Frame::new(renderer, bounds.size());
        let pill = self.pill;
        let r = self.radius;
        // How strong the ring and its bloom are, how wide, which way the colours run.
        let level = self.level;
        let (width, bloom, angle, alpha) = match self.mode {
            Mode::Idle => {
                let breath = if self.reduced { 0.0 } else { aura::breath(self.t) };
                (1.0, 0.0, 0.0, 0.20 + 0.10 * breath)
            }
            Mode::Listening => {
                // Snap in over 160 ms.
                let snap = if self.reduced { 1.0 } else { aura::cubic_bezier(0.2, 0.9, 0.3, 1.0, self.phase_t / 0.160) };
                (aura::stroke_width(level), aura::bloom(level) * snap, if self.reduced { 0.0 } else { self.t * 0.6 }, 0.45 + 0.55 * snap)
            }
            Mode::Transcribing => (2.0, 0.25 + 0.4 * level, 0.0, 1.0),
            Mode::Thinking => (2.0, 0.30, if self.reduced { 0.0 } else { aura::sweep_angle(self.phase_t) }, 1.0),
            Mode::Done => (2.0 + 1.5 * self.done, 0.65 * self.done, 0.0, 0.35 + 0.65 * self.done),
            Mode::Error => {
                let fade = (self.error_t / 0.6).clamp(0.0, 1.0);
                (2.0, 0.45 * (1.0 - fade) + 0.12, 0.0, 0.75 + 0.25 * (1.0 - fade))
            }
        };
        let colors = if self.mode == Mode::Idle { self.accent } else { self.colors };
        // Soft bloom: rings growing outwards, fading.
        if bloom > 0.005 {
            let layers = 9;
            for i in 1..=layers {
                let f = i as f32 / (layers + 1) as f32;
                let a = bloom * 0.6 * (1.0 - f).powf(1.6);
                let ring = grow(pill, f * 16.0);
                frame.stroke(&rounded(ring, r + f * 16.0), Stroke { style: self.gradient(&colors, angle, a), width: 2.4, ..Default::default() });
            }
        }
        frame.fill(&rounded(pill, r), self.surface);
        // Transcribing: a ripple running inwards from the border.
        if self.mode == Mode::Transcribing && !self.reduced {
            let p = aura::ripple(self.phase_t);
            let inset = p * (pill.height / 2.0).min(14.0);
            let inner = grow(pill, -inset);
            frame.stroke(&rounded(inner, (r - inset).max(2.0)), Stroke { style: self.gradient(&colors, 0.0, 0.55 * (1.0 - p)), width: 1.5, ..Default::default() });
        }
        // The ring.
        let half = width / 2.0;
        frame.stroke(&rounded(grow(pill, -half), (r - half).max(0.0)), Stroke { style: self.gradient(&colors, angle, alpha), width, ..Default::default() });
        // The state dot left of the field: listening grows with the voice, thinking spins.
        let dot = Point::new(pill.x + 22.0, pill.y + 26.0);
        match self.mode {
            Mode::Thinking if !self.reduced => {
                let a = aura::sweep_angle(self.phase_t);
                let arc = Path::new(|b| {
                    b.arc(canvas::path::Arc { center: dot, radius: 6.0, start_angle: a.into(), end_angle: (a + 4.2).into() });
                });
                frame.stroke(&arc, Stroke { style: canvas::Style::Solid(color(colors.primary, 1.0)), width: 2.0, ..Default::default() });
            }
            _ => {
                let (radius, a) = match self.mode {
                    Mode::Listening => (4.0 + 3.0 * level, 1.0),
                    Mode::Transcribing | Mode::Thinking => (5.0, 0.9),
                    Mode::Done | Mode::Error => (5.0, 1.0),
                    Mode::Idle => (4.0, 0.55),
                };
                frame.fill(&Path::circle(dot, radius), color(colors.primary, a));
            }
        }
        vec![frame.into_geometry()]
    }
}

// --------------------------------------------------------------------------- application

impl cosmic::Application for Overlay {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = Flags;
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, flags: Flags) -> (Self, Task<Message>) {
        let settings = settings::read();
        maprisma_voice::i18n::set_language(&settings.language);
        corners::set_current(&settings.corners);
        look::reload();
        let reduced = settings::reduced_motion(settings.motion, || settings::desktop_reduces_motion(look::desktop()));
        let now = Instant::now();
        let mut app = Overlay {
            core,
            flags,
            settings,
            reduced,
            machine: Machine::default(),
            started: now,
            output_wanted: std::env::var("MAPRISMA_DICTATION_OUTPUT").ok().filter(|s| !s.is_empty()),
            output: None,
            surface: None,
            catcher: None,
            zone: None,
            voice_gen: 0,
            voice_on: None,
            controls: None,
            queued: vec![],
            ask_gen: 0,
            ask: None,
            helper_name: String::new(),
            last: crate::last::load(),
            envelope: Envelope::default(),
            target_level: 0.0,
            last_frame: now,
            phase_since: now,
            done_at: None,
            error_at: None,
            size_from: COMPACT,
            size_to: COMPACT,
            size_since: now,
            status: None,
            status_gen: 0,
            capturing: false,
            body_height: None,
        };
        let theme = corners::set_app_theme(&app.settings.corners);
        let open = if app.output_wanted.is_some() {
            // Wait for that output to be announced (a second at most, then the active one).
            Task::perform(tokio::time::sleep(Duration::from_millis(1000)), |_| cosmic::Action::App(Message::OpenFallback))
        } else {
            app.open(IcedOutput::Active)
        };
        (app, Task::batch([theme, open]))
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            cosmic::iced::time::every(Duration::from_secs(5)).map(|_| Message::LookTick),
            Subscription::run(|| {
                cosmic::iced::stream::channel(8, async |mut out: cosmic::iced::futures::channel::mpsc::Sender<Message>| {
                    use cosmic::iced::futures::SinkExt;
                    if let Some(mut commands) = crate::instance::take_commands() {
                        while let Some(c) = commands.recv().await {
                            let _ = out.send(Message::Command(c)).await;
                        }
                    }
                    std::future::pending::<()>().await;
                })
            }),
            cosmic::iced::event::listen_with(|event, _status, id| match event {
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    use cosmic::iced::keyboard::{Key, key::Named};
                    if shot::is_shot_key(&key, modifiers) {
                        return Some(Message::ShotKey(id));
                    }
                    match key.as_ref() {
                        Key::Named(Named::Escape) => Some(Message::Escape),
                        Key::Character(c) if modifiers.control() && modifiers.shift() && c.eq_ignore_ascii_case("c") => Some(Message::CopyAnswer),
                        Key::Character(c) if modifiers.control() && !modifiers.shift() && c.eq_ignore_ascii_case("m") => Some(Message::ToggleMic),
                        _ => None,
                    }
                }
                cosmic::iced::Event::PlatformSpecific(cosmic::iced::event::PlatformSpecific::Wayland(WaylandEvent::Output(OutputEvent::Created(Some(info)), output))) => {
                    Some(Message::Output(info.name.clone().unwrap_or_default(), output))
                }
                _ => None,
            }),
        ];
        if let Some(end) = self.voice_on {
            let key = (self.voice_gen, end);
            let session = if self.flags.demo { Subscription::run_with(key, voice::demo) } else { Subscription::run_with(key, voice::session) };
            subs.push(session.with(self.voice_gen).map(|(generation, event)| Message::Voice(generation, event)));
        }
        if self.animating() {
            subs.push(cosmic::iced::time::every(Duration::from_millis(16)).map(|_| Message::Frame));
        } else if !self.reduced && self.surface.is_some() && matches!(self.machine.phase, Phase::Idle | Phase::Done) {
            // The idle breath is slow: a few frames a second are enough.
            subs.push(cosmic::iced::time::every(Duration::from_millis(100)).map(|_| Message::Frame));
        }
        Subscription::batch(subs)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Output(name, output) => {
                if self.surface.is_none() && self.output_wanted.as_deref() == Some(name.as_str()) {
                    return self.open(IcedOutput::Output(output));
                }
            }
            Message::OpenFallback => return self.open(IcedOutput::Active),
            Message::Command(Command::Toggle | Command::Close) => return self.run(vec![Effect::Close]),
            Message::Command(Command::Release) => {
                let held = self.started.elapsed().as_millis() as u64;
                return self.handle(Event::Released { held_ms: held });
            }
            Message::Voice(generation, event) => {
                if generation != self.voice_gen || self.voice_on.is_none() {
                    return Task::none();
                }
                return match event {
                    VoiceEvent::Ready(controls) => {
                        for c in self.queued.drain(..) {
                            let _ = controls.0.send(c);
                        }
                        self.controls = Some(controls);
                        Task::none()
                    }
                    VoiceEvent::Level(rms) => {
                        self.target_level = aura::level_from_rms(rms);
                        Task::none()
                    }
                    VoiceEvent::Partial(text) => {
                        self.machine.partial = text;
                        Task::none()
                    }
                    VoiceEvent::State(s) => {
                        if s != "listening" {
                            self.target_level = 0.0;
                        }
                        self.handle(Event::VoiceState(s))
                    }
                    VoiceEvent::Final(text) => {
                        self.voice_on = None;
                        self.controls = None;
                        self.target_level = 0.0;
                        // The machine still knows the session: it sends the question on.
                        self.handle(Event::VoiceFinal(text))
                    }
                    VoiceEvent::Error(message) => {
                        self.status = Some(message);
                        Task::none()
                    }
                    VoiceEvent::Closed(reason) => {
                        let message = self.status.take();
                        self.voice_on = None;
                        self.controls = None;
                        self.target_level = 0.0;
                        self.handle(Event::VoiceClosed { reason, message })
                    }
                    VoiceEvent::Failed(message) => {
                        self.voice_on = None;
                        self.controls = None;
                        self.handle(Event::VoiceFailed(message))
                    }
                };
            }
            Message::Input(text) => return self.handle(Event::Typed(text)),
            Message::Submit => return self.handle(Event::Submit),
            Message::Escape => {
                let task = self.handle(Event::Escape);
                return Task::batch([task, cosmic::widget::text_input::focus(INPUT_ID.clone())]);
            }
            Message::ClickOutside => return self.handle(Event::ClickOutside),
            Message::ToggleMic => return self.handle(Event::ToggleMic),
            Message::CopyAnswer => {
                if !self.machine.answer.is_empty() {
                    let status = self.set_status(tr("Copied"));
                    return Task::batch([cosmic::iced::clipboard::write(self.machine.answer.clone()), status]);
                }
            }
            Message::Answer(generation, result, name) => {
                if generation != self.ask_gen {
                    return Task::none();
                }
                self.ask = None;
                if !name.is_empty() {
                    self.helper_name = name;
                }
                if let Ok(answer) = &result {
                    let e = Exchange { question: self.machine.question.clone(), answer: answer.clone(), at: crate::last::now() };
                    crate::last::save(&e);
                    self.last = Some(e);
                }
                return self.handle(Event::Answer(result));
            }
            Message::Frame => {
                let now = Instant::now();
                let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.1);
                self.last_frame = now;
                self.envelope.step(self.target_level, dt);
            }
            Message::LookTick => {
                let fresh = settings::read();
                let mut restyle = fresh.corners != self.settings.corners;
                if fresh.language != self.settings.language {
                    maprisma_voice::i18n::set_language(&fresh.language);
                }
                corners::set_current(&fresh.corners);
                self.settings = fresh;
                restyle |= look::refresh();
                if restyle {
                    return corners::set_app_theme(&self.settings.corners);
                }
            }
            Message::ShotKey(id) => {
                if self.settings.screenshot_key && Some(id) == self.surface && !self.capturing {
                    self.capturing = true;
                    return window::screenshot(id).map(|picture| cosmic::Action::App(Message::Captured(picture)));
                }
            }
            Message::Captured(picture) => {
                if !std::mem::take(&mut self.capturing) {
                    return Task::none();
                }
                let (width, height) = (picture.size.width, picture.size.height);
                return off_thread(move || shot::save_screenshot(SHOT_FOLDER, &picture.rgba, width, height), Message::ShotSaved);
            }
            Message::ShotSaved(result) => {
                let text = match result {
                    Ok(saved) => trf("Screenshot saved: {0}", &[&saved.shown]),
                    Err(reason) => trf("The screenshot could not be saved: {0}", &[&reason]),
                };
                return self.set_status(text);
            }
            Message::BodySize(size) => {
                if self.body_height != Some(size.height) {
                    self.body_height = Some(size.height);
                    return self.update_zone(false);
                }
            }
            Message::ClearStatus(generation) => {
                if generation == self.status_gen {
                    self.status = None;
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        space::horizontal().width(Length::Fixed(1.0)).into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        if Some(id) == self.catcher {
            return mouse_area(container(space::horizontal()).width(Length::Fill).height(Length::Fill)).on_press(Message::ClickOutside).into();
        }
        self.pill()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        let theme = cosmic::theme::active();
        Some(cosmic::iced::theme::Style { background_color: Color::TRANSPARENT, text_color: theme.cosmic().on_bg_color().into(), icon_color: theme.cosmic().on_bg_color().into() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heights_grow_with_the_answer_and_stop_at_the_maximum() {
        let short = estimate_height(&markdown::parse("Noon."), true, EXPANDED_WIDTH);
        assert!(short > COMPACT.1 && short < 200.0, "{short}");
        let long = estimate_height(&markdown::parse(&"word ".repeat(600)), true, EXPANDED_WIDTH);
        assert_eq!(long, MAX_HEIGHT);
        assert_eq!(tail("abcdef", 4), "…def");
        assert_eq!(tail("abc", 4), "abc");
    }
}
