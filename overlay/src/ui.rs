//! The overlay: a layer-shell surface at the top centre (48 px below the top edge, keyboard on
//! demand) holding the pill, its aura ring and the answer; plus, while closing by a click
//! would lose nothing, a transparent full-screen layer under it that catches clicks outside.
//! The decisions are in state.rs, the ring's maths in aura.rs; this file draws and wires.

use crate::actions::exec::{self, Outcome, Target};
use crate::actions::{self, State as ItemState, Turn, policy, schema};
use crate::answer::{self, AutoClose, View};
use crate::aura::{self, AuraColors, Envelope, Tone};
use crate::helper::{self, Exchange};
use crate::instance::Command;
use crate::markdown::{self, Block};
use crate::settings::{self, Settings};
use crate::state::{Effect, End, Event, Machine, Phase};
use crate::voice::{self, Control, Controls, SpeakEvent, VoiceEvent};
use crate::{corners, fonts, look, shot};
use cosmic::app::{Core, Task};
use cosmic::cctk::sctk::reexports::client::protocol::wl_output::WlOutput;
use cosmic::iced::event::wayland::{Event as WaylandEvent, OutputEvent};
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::{IcedMargin, IcedOutput, SctkLayerSurfaceSettings};
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface, set_input_zone, set_size,
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
static DEBUG: LazyLock<bool> = LazyLock::new(|| std::env::var_os("MAPRISMA_DICTATION_DEBUG").is_some());
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
/// A chip's height with its gap (the height estimate before the body is measured).
const CHIP: f32 = 38.0;
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
    /// `--ask TEXT`: open without listening and send this question at once.
    pub ask: Option<String>,
    /// `--wake`: the wake phrase opened it; listen until silence, whatever `listen`/`end` say.
    pub wake: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// An output was announced (its name, logical size).
    Output(String, Option<(f32, f32)>, WlOutput),
    /// An output's size became known or changed.
    OutputInfo(String, Option<(f32, f32)>),
    /// The chips of a command were shown long enough: close (the auto-close's generation).
    AutoClose(u64),
    /// A key press the overlay has no use for (it still keeps an auto-close from happening).
    Activity,
    /// The pointer came over the pill (keeps it open).
    PointerIn,
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
    /// The windows the answer's actions aim at were looked up.
    Prepared(u64, Vec<Target>),
    /// An action finished (turn, item).
    ActionDone(u64, usize, Outcome),
    /// The Run / Cancel buttons on the amber card (a click is a physical confirmation too).
    ConfirmClicked,
    CancelClicked,
    /// Undo for this item, or (Ctrl+Z) the newest one.
    Undo(Option<usize>),
    UndoDone(u64, usize, Result<(), String>),
    /// One of several matching apps was picked.
    Pick(usize, usize),
    Speak(u64, SpeakEvent),
    /// `--ask TEXT`, sent once the overlay has settled.
    AskNow(String),
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
    // actions
    /// The answer as shown: without the actions block.
    shown: String,
    turn: Turn,
    turn_gen: u64,
    /// The request carries outside text (pasted, or a follow-up of a weather lookup).
    tainted: bool,
    // spoken answers
    speak_gen: u64,
    speak: Option<(String, String)>,
    speaking_since: Option<Instant>,
    // how the answer shows (answer.rs)
    view: View,
    /// The outputs' names and logical sizes (for the reading view's size).
    outputs: Vec<(String, (f32, f32))>,
    /// The reading view's width and largest height on this output.
    reading: (f32, f32),
    /// The layer surface's size now (it grows for the reading view, never shrinks).
    surface_size: (f32, f32),
    /// A command's auto-close: when it was armed, how long, whether something can be undone.
    closing: Option<Closing>,
    close_gen: u64,
    /// A key, the pointer or typing kept this answer open.
    close_kept: bool,
}

#[derive(Debug, Clone, Copy)]
struct Closing {
    since: Instant,
    millis: u64,
    undo: bool,
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

/// Blocking work on a thread of its own; `done` turns its result into a message.
fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static, done: impl Fn(T) -> Message + Send + 'static) -> Task<Message> {
    let (sender, receiver) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    Task::perform(receiver, move |result| match result {
        Ok(value) => cosmic::Action::App(done(value)),
        Err(_) => cosmic::Action::App(Message::Frame),
    })
}

/// The answer's height in the pill (a rough line count; the body scrolls when it is more), at
/// most `max`; `reading`: the reading view's larger type.
pub fn estimate_height(blocks: &[Block], question: bool, width: f32, max: f32, reading: bool) -> f32 {
    let (char_width, line) = if reading { (7.9, 23.0) } else { (7.4, 20.5) };
    let per_line = ((width - 2.0 * if reading { 24.0 } else { 16.0 }) / char_width).max(20.0) as usize;
    let lines = |t: &str| t.lines().map(|l| l.chars().count().div_ceil(per_line).max(1)).sum::<usize>() as f32;
    let mut h = COMPACT.1 + 1.0 + 12.0; // the field, a divider, padding
    if question {
        h += 22.0;
    }
    for b in blocks {
        h += 8.0 + match b {
            Block::Heading(t) => 22.0 * lines(t),
            Block::Paragraph(t) => line * lines(t),
            Block::Item(_, t) => line * lines(t),
            Block::Code(t) => 18.0 * t.lines().count().max(1) as f32 + 16.0,
        };
    }
    (h + 12.0 + 26.0).clamp(COMPACT.1, max) // the footer
}

impl Overlay {
    fn now(&self) -> f32 {
        self.started.elapsed().as_secs_f32()
    }

    fn layer_settings(&self, id: window::Id) -> SctkLayerSurfaceSettings {
        SctkLayerSurfaceSettings {
            id,
            layer: Layer::Overlay,
            keyboard_interactivity: if self.settings.keyboard_exclusive { KeyboardInteractivity::Exclusive } else { KeyboardInteractivity::OnDemand },
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
        let listen = if self.flags.wake {
            // Spoken to: always listen, ending on silence (there is no key to release).
            Some(End::Silence)
        } else if !self.settings.listen && !self.flags.hold {
            None
        } else if self.flags.hold {
            Some(End::Release)
        } else if self.settings.end_silence {
            Some(End::Silence)
        } else {
            Some(End::Release)
        };
        let listen = if self.flags.ask.is_some() { None } else { listen };
        let effects = self.machine.handle(Event::Opened { listen });
        // --ask: once the surface has settled (its first configure and output scale; a body
        // measured in the very first frames can race libcosmic's scale change and crash it).
        let ask_now = match self.flags.ask.take() {
            Some(question) => Task::perform(tokio::time::sleep(Duration::from_millis(500)), move |_| cosmic::Action::App(Message::AskNow(question.clone()))),
            None => Task::none(),
        };
        let zone = self.update_zone(true);
        Task::batch([get_layer_surface(self.layer_settings(id)), zone, cosmic::widget::text_input::focus(INPUT_ID.clone()), self.run(effects), ask_now])
    }

    /// The pill's size for what shows now.
    fn target_size(&self) -> (f32, f32) {
        let m = &self.machine;
        let expanded = !m.answer.is_empty() || matches!(m.phase, Phase::Thinking | Phase::Error(_)) || m.note.is_some();
        if !expanded {
            return COMPACT;
        }
        let (width, max) = self.view_size();
        // The field, divider, gap, body, footer: the body as measured once laid out.
        let chrome = COMPACT.1 + 1.0 + 10.0 + 4.0 + self.footer_height();
        let height = match (self.body_height, &m.phase) {
            (Some(body), _) => chrome + body,
            (None, Phase::Thinking) => chrome + 21.0,
            (None, Phase::Error(e)) => chrome + 21.0 + 22.0 * (e.chars().count() as f32 / 52.0).ceil().max(1.0),
            // Until measured: the text, and a chip's height for each action (cards are measured).
            (None, _) if !m.answer.is_empty() => estimate_height(&markdown::parse(&self.shown), !m.question.is_empty(), width, max, self.view == View::Reading) + CHIP * self.turn.items.len() as f32,
            (None, _) => chrome + 21.0,
        };
        (width, height.min(max))
    }

    /// The pill's width and largest height for the answer's view.
    fn view_size(&self) -> (f32, f32) {
        if self.view == View::Reading { self.reading } else { (EXPANDED_WIDTH, MAX_HEIGHT) }
    }

    /// The logical size of the output the overlay is on, when known.
    fn output_size(&self) -> Option<(f32, f32)> {
        let focused = if self.outputs.len() > 1 && self.output_wanted.is_none() && look::desktop() == look::Desktop::Hyprland { focused_hyprland_output() } else { None };
        answer::pick_output(&self.outputs, self.output_wanted.as_deref(), focused.as_deref())
    }

    fn shows_footer(&self) -> bool {
        !self.machine.answer.is_empty() || matches!(self.machine.phase, Phase::Error(_))
    }

    fn footer_height(&self) -> f32 {
        if self.shows_footer() { 30.0 } else { 8.0 }
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
        // The reading view needs a bigger surface: grow it once to the view's largest size. Square:
        // libcosmic (95236a9) answers set_size with a configure of width × width (its height takes
        // the width), and tiny-skia then panics on the buffer of the real, different size.
        let mut grow = Task::none();
        if target.0 + 2.0 * MARGIN > self.surface_size.0 + 0.5 || target.1 + 2.0 * MARGIN > self.surface_size.1 + 0.5 {
            let (w, h) = self.view_size();
            let side = self.surface_size.0.max(self.surface_size.1).max(w.max(target.0) + 2.0 * MARGIN).max(h.max(target.1) + 2.0 * MARGIN).ceil();
            self.surface_size = (side, side);
            if let Some(id) = self.surface {
                grow = set_size(id, Some(self.surface_size.0.ceil() as u32), Some(self.surface_size.1.ceil() as u32));
            }
        }
        let (w, h) = (self.size_from.0.max(self.size_to.0), self.size_from.1.max(self.size_to.1));
        let zone = Rectangle::new(Point::new((self.surface_size.0 - w) / 2.0 - 6.0, MARGIN - 6.0), Size::new(w + 12.0, h + 12.0));
        if self.zone == Some(zone) {
            return grow;
        }
        self.zone = Some(zone);
        match self.surface {
            Some(id) => Task::batch([grow, set_input_zone(id, Some(vec![zone]))]),
            None => grow,
        }
    }

    /// Arms the auto-close once a command's actions all ran (answer.rs decides); anything else
    /// that happens meanwhile (a key, the pointer, typing, a new question) disarms it.
    fn check_auto_close(&mut self) -> Task<Message> {
        if self.closing.is_some() || self.close_kept || self.machine.phase != Phase::Done {
            return Task::none();
        }
        match answer::auto_close(self.view, &self.turn.items) {
            AutoClose::After { millis, undo } => {
                self.closing = Some(Closing { since: Instant::now(), millis, undo });
                self.close_gen += 1;
                let generation = self.close_gen;
                Task::perform(tokio::time::sleep(Duration::from_millis(millis)), move |_| cosmic::Action::App(Message::AutoClose(generation)))
            }
            AutoClose::Pending | AutoClose::Stay => Task::none(),
        }
    }

    /// The user did something: this answer stays open.
    fn keep_open(&mut self) {
        self.close_kept = true;
        if self.closing.take().is_some() {
            self.close_gen += 1;
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
                    self.end_turn();
                    tasks.push(self.ask(question));
                }
                Effect::Confirm => {
                    if let Some(i) = self.turn.confirm() {
                        tasks.push(self.execute(i));
                    }
                    self.sync_card();
                }
                Effect::CancelConfirm => {
                    self.turn.cancel(crate::last::now());
                    tasks.push(self.pump());
                }
                Effect::StopSpeaking => tasks.push(self.stop_speaking()),
                Effect::CancelAsk => self.cancel_ask(),
                Effect::FlashDone => self.done_at = Some(Instant::now()),
                Effect::FlashError => self.error_at = Some(Instant::now()),
                Effect::Close => {
                    self.cancel_ask();
                    self.end_turn();
                    let stop = if self.speak.is_some() || self.machine.speaking { Some(self.stop_speaking()) } else { None };
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
                    if let Some(stop) = stop {
                        close.push(stop);
                    }
                    // Give the Cancel (and StopSpeaking) a moment to reach the service, then leave.
                    close.push(Task::perform(tokio::time::sleep(Duration::from_millis(if self.speak_gen > 0 { 300 } else { 60 })), |_| cosmic::Action::App(Message::Frame)).chain(cosmic::iced::exit()));
                    return Task::batch(close);
                }
            }
        }
        tasks.push(self.update_zone(false));
        tasks.push(self.sync_catcher());
        Task::batch(tasks)
    }

    fn note_output(&mut self, name: String, size: Option<(f32, f32)>) {
        let Some(size) = size else { return };
        match self.outputs.iter_mut().find(|(n, _)| *n == name) {
            Some(o) => o.1 = size,
            None => self.outputs.push((name, size)),
        }
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
        self.tainted = context.as_ref().is_some_and(|e| e.tainted) || policy::looks_pasted(&question);
        let actions = self.settings.actions;
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
                return (Err(tr("No AI helper is installed. Choose one in Maprisma Settings → AI.")), String::new());
            };
            let name = chosen.installed.helper.name.clone();
            let prompt = helper::build_prompt(&question, context.as_ref(), &language, &desktop, actions);
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

    fn context(&self) -> exec::Context {
        exec::Context { desktop: look::desktop(), language: ui_language(&self.settings.language), weather_location: self.settings.weather_location.clone() }
    }

    fn flush_log(&mut self) {
        for e in self.turn.log.drain(..) {
            actions::log::append(&e);
        }
    }

    fn sync_card(&mut self) {
        self.machine.confirming = self.turn.confirming().is_some();
    }

    /// A new question or closing: what is left of the last answer's actions doesn't run.
    fn end_turn(&mut self) {
        self.turn.cancel_all(crate::last::now());
        self.flush_log();
        self.turn = Turn::default();
        self.turn_gen += 1;
        self.shown.clear();
        self.sync_card();
        self.closing = None;
        self.close_gen += 1;
        self.close_kept = false;
    }

    /// Starts the next action (or shows its card).
    fn pump(&mut self) -> Task<Message> {
        self.flush_log();
        let task = match self.turn.advance() {
            actions::Next::Run(i) => self.execute(i),
            _ => Task::none(),
        };
        self.flush_log();
        self.sync_card();
        self.save_last();
        Task::batch([task, self.update_zone(false), self.sync_catcher(), self.check_auto_close()])
    }

    fn execute(&self, i: usize) -> Task<Message> {
        let Some(item) = self.turn.items.get(i) else { return Task::none() };
        let (Some(action), target, ctx, generation) = (item.action.clone(), item.target.clone(), self.context(), self.turn_gen) else { return Task::none() };
        blocking(move || exec::execute(&action, &target, &ctx), move |outcome| Message::ActionDone(generation, i, outcome))
    }

    /// The last exchange with what the actions did (a follow-up's helper sees the results).
    fn save_last(&mut self) {
        if self.flags.demo || self.machine.answer.is_empty() || self.machine.question.is_empty() {
            return;
        }
        let context = self.turn.context();
        let answer = match (self.shown.is_empty(), context.is_empty()) {
            (_, true) => self.shown.clone(),
            (true, false) => context,
            (false, false) => format!("{}\n\n{context}", self.shown),
        };
        let e = Exchange { question: self.machine.question.clone(), answer, tainted: self.turn.taints_follow_up(self.tainted, &self.machine.question), at: crate::last::now() };
        if self.last.as_ref() != Some(&e) {
            crate::last::save(&e);
            self.last = Some(e);
        }
    }

    fn stop_speaking(&mut self) -> Task<Message> {
        self.speak = None;
        self.speaking_since = None;
        self.machine.speaking = false;
        Task::perform(voice::stop_speaking(), |_| cosmic::Action::App(Message::Frame))
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
        let counting = self.closing.is_some_and(|c| c.undo) && !self.reduced;
        let flashing = counting
            || self.done_at.is_some_and(|t| t.elapsed().as_secs_f32() < aura::DONE_FLASH)
            || self.error_at.is_some_and(|t| t.elapsed().as_secs_f32() < 0.6)
            || self.size_since.elapsed().as_secs_f32() < aura::MORPH;
        if self.reduced {
            return flashing || self.voice_on.is_some() || self.envelope.value > 0.01;
        }
        flashing || self.machine.speaking || matches!(self.machine.phase, Phase::Listening | Phase::Transcribing | Phase::Thinking) || self.envelope.value > 0.01
    }

    // ----------------------------------------------------------------------------- drawing

    fn aura(&self) -> AuraCanvas {
        let theme = cosmic::theme::active();
        let c = theme.cosmic();
        let dark = c.is_dark;
        let accent = rgb(c.accent_color().into());
        let t = self.now();
        let phase_t = match self.speaking_since {
            Some(since) if self.machine.speaking => since.elapsed().as_secs_f32(),
            _ => self.phase_since.elapsed().as_secs_f32(),
        };
        let done = self.done_at.map(|d| aura::done_flash(d.elapsed().as_secs_f32())).unwrap_or(0.0);
        let error_t = self.error_at.map(|e| e.elapsed().as_secs_f32());
        let (tone, mode) = match &self.machine.phase {
            Phase::Listening => (Tone::Accent, Mode::Listening),
            Phase::Transcribing => (Tone::Accent, Mode::Transcribing),
            Phase::Thinking => (Tone::Accent, Mode::Thinking),
            Phase::Error(_) => (Tone::Error, Mode::Error),
            Phase::Done if done > 0.0 => (Tone::Done, Mode::Done),
            _ if self.machine.speaking => (Tone::Accent, Mode::Speaking),
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
        // Opaque: text behind the pill must not show through the answer (Hyprland showed it at 92 %).
        surface.a = 1.0;
        let countdown = match self.closing {
            Some(c) if c.undo && !self.reduced => Some((1.0 - c.since.elapsed().as_secs_f32() * 1000.0 / c.millis as f32).clamp(0.0, 1.0)),
            _ => None,
        };
        AuraCanvas {
            size: Size::new(w, h),
            shake,
            countdown,
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
            // One line: the newest words, the start scrolls away.
            let room = if self.size_to.0 > COMPACT.0 { 44 } else { 32 };
            let partial = text::body(tail(&m.partial, room)).size(15.0).wrapping(cosmic::iced::widget::text::Wrapping::None).class(theme::Text::Custom(partial_text));
            cosmic::iced::widget::stack![input, container(partial).padding([0, 8]).width(Length::Fill).height(Length::Fill).clip(true).align_y(cosmic::iced::Alignment::Center)].into()
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
        let reading = self.view == View::Reading;
        if !m.answer.is_empty() && m.phase != Phase::Thinking {
            let blocks = markdown::parse_raw(&self.shown);
            if reading {
                // Its own gaps (answer::gap_before), not the column's even 8 px.
                let mut text_col = cosmic::iced::widget::Column::new().width(Length::Fill);
                for (i, block) in blocks.iter().enumerate() {
                    let gap = answer::gap_before(i.checked_sub(1).map(|p| &blocks[p]), block);
                    if gap > 0.0 {
                        text_col = text_col.push(space::vertical().height(Length::Fixed(gap)));
                    }
                    text_col = text_col.push(block_view(block, Type::Reading { lead: answer::is_lead(&blocks, i) }));
                }
                if !blocks.is_empty() {
                    col = col.push(text_col);
                }
            } else {
                for block in &blocks {
                    col = col.push(block_view(block, Type::Compact));
                }
            }
            for (i, item) in self.turn.items.iter().enumerate() {
                col = col.push(self.item_view(i, item));
            }
            any = true;
        }
        if let Some(note) = &m.note {
            col = col.push(text::caption(tr(note)).class(theme::Text::Custom(dim_text)));
            any = true;
        }
        let padding = if reading { [0, 24, 8, 24] } else { [0, 20, 4, 16] };
        let measured = cosmic::iced::widget::sensor(container(col).padding(padding).width(Length::Fixed(self.size_to.0)))
            .on_show(Message::BodySize)
            .on_resize(Message::BodySize);
        // Shrink (not Fill): the sensor then measures the answer itself, not the space it has;
        // the room it may take is set by pill() (the footer stays visible).
        any.then(|| scrollable(measured).height(Length::Shrink).into())
    }

    fn footer(&self) -> Element<'_, Message> {
        let left = self.status.clone().unwrap_or_else(|| self.helper_name.clone());
        let hints = if let Some(c) = self.closing.filter(|c| c.undo) {
            let left = answer::seconds_left(c.millis, c.since.elapsed().as_millis() as u64).max(1);
            trf("Ctrl+Z undoes · closes in {0} s", &[&left.to_string()])
        } else if self.machine.confirming {
            tr("Enter runs · Esc cancels")
        } else if self.machine.speaking {
            tr("Esc stops reading aloud")
        } else if self.machine.answer.is_empty() {
            tr("Enter sends · Ctrl+M talks · Esc closes")
        } else if self.turn.undoable().is_some() {
            tr("Ctrl+Z undoes · Enter asks · Esc closes")
        } else {
            tr("Enter asks · Ctrl+Shift+C copies · Esc closes")
        };
        row![
            text::caption(left).class(theme::Text::Custom(dim_text)),
            space::horizontal().width(Length::Fill),
            text::caption(hints).class(theme::Text::Custom(dim_text)),
        ]
        .padding([4, 16, 8, 16])
        .into()
    }

    /// One action: a chip, the amber card, the weather card, a command's output or app choices.
    fn item_view(&self, i: usize, item: &actions::Item) -> Element<'_, Message> {
        let what = item.action.as_ref().map(actions::describe).unwrap_or_else(|| tr("An action this overlay can't read"));
        let chip = |mark: &'static str, label: String, tone: fn(&cosmic::Theme) -> iced_text::Style, undo: Option<usize>| -> Element<'_, Message> {
            let mut r = row![text::body(mark).class(theme::Text::Custom(tone)).width(Length::Fixed(16.0)), text::body(label).width(Length::Fill)].spacing(4).align_y(cosmic::iced::Alignment::Center);
            if let Some(i) = undo {
                r = r.push(cosmic::widget::button::text(tr("Undo")).on_press(Message::Undo(Some(i))));
            }
            container(r).padding([4, 8]).width(Length::Fill).class(theme::Container::custom(chip_box)).into()
        };
        match &item.state {
            ItemState::Waiting => chip("·", what, dim_text, None),
            ItemState::Running => chip("…", what, dim_text, None),
            ItemState::Done { text, undo } => chip("▸", text.clone(), accent_text, undo.as_ref().map(|_| i)),
            ItemState::Undone(text) => chip("↶", trf("Undone: {0}", &[text]), dim_text, None),
            ItemState::Failed(e) => chip("✕", format!("{what}: {e}"), error_text, None),
            ItemState::Unsupported(e) => chip("!", format!("{what}: {e}"), warning_text, None),
            ItemState::Blocked(why) => chip("⊘", format!("{} {what}. {}", tr("Blocked:"), tr(why)), error_text, None),
            ItemState::Rejected(_) => chip("⊘", tr("The helper proposed an action this overlay can't read."), dim_text, None),
            ItemState::Cancelled => chip("–", trf("Cancelled: {0}", &[&what]), dim_text, None),
            ItemState::Confirm => self.card_view(item, what),
            ItemState::Weather(w) => weather_view(w),
            ItemState::Output { ok, text, output } => {
                let mut c = cosmic::iced::widget::Column::new().spacing(4).push(row![
                    text::body(if *ok { "▸" } else { "✕" }).class(theme::Text::Custom(if *ok { accent_text } else { error_text })).width(Length::Fixed(16.0)),
                    text::body(text.clone())
                ]);
                if !output.is_empty() {
                    let shown: String = output.chars().take(4000).collect();
                    c = c.push(container(text::body(shown).font(cosmic::font::mono()).size(12.5)).padding(8).width(Length::Fill).class(theme::Container::custom(code_box)));
                }
                c.into()
            }
            ItemState::Choices(list) => {
                let mut r = row![].spacing(8).align_y(cosmic::iced::Alignment::Center);
                for (n, app) in list.iter().enumerate() {
                    r = r.push(cosmic::widget::button::standard(app.name.clone()).on_press(Message::Pick(i, n)));
                }
                column![text::caption(tr("Several apps fit. Which one?")).class(theme::Text::Custom(dim_text)), scrollable(r).direction(cosmic::iced::widget::scrollable::Direction::Horizontal(Default::default()))].spacing(4).into()
            }
        }
    }

    /// The amber confirmation card: what would run, [Esc] Cancel, [Enter] Run.
    fn card_view(&self, item: &actions::Item, what: String) -> Element<'_, Message> {
        let mut c = cosmic::iced::widget::Column::new().spacing(6).push(row![text::body("!").class(theme::Text::Custom(warning_text)).width(Length::Fixed(16.0)), text::heading(what)]);
        match &item.action {
            Some(schema::Action::RunCommand { argv, why }) => {
                c = c.push(container(text::body(shell_words(argv)).font(cosmic::font::mono()).size(12.5)).padding(8).width(Length::Fill).class(theme::Container::custom(code_box)));
                if !why.is_empty() {
                    c = c.push(text::caption(why.clone()).class(theme::Text::Custom(dim_text)));
                }
                c = c.push(text::caption(tr("Runs in your home folder without a shell, for at most 60 s.")).class(theme::Text::Custom(dim_text)));
            }
            Some(schema::Action::CloseWindow | schema::Action::MoveWindow { .. }) => {
                let window = match &item.target.window {
                    Some(w) if !w.title.is_empty() => format!("{} — {}", w.class, w.title),
                    Some(w) => w.class.clone(),
                    None => tr("the active window"),
                };
                c = c.push(text::caption(window).class(theme::Text::Custom(dim_text)));
            }
            _ if self.tainted || self.turn.read_outside() => {
                c = c.push(text::caption(tr("This request read outside text, so it asks first.")).class(theme::Text::Custom(dim_text)));
            }
            _ => {}
        }
        c = c.push(
            row![
                space::horizontal().width(Length::Fill),
                cosmic::widget::button::text(format!("Esc  {}", tr("Cancel"))).on_press(Message::CancelClicked),
                cosmic::widget::button::suggested(format!("Enter  {}", tr("Run"))).on_press(Message::ConfirmClicked),
            ]
            .spacing(8)
            .align_y(cosmic::iced::Alignment::Center),
        );
        container(c).padding(12).width(Length::Fill).class(theme::Container::custom(amber_card)).into()
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
                let room = (h - COMPACT.1 - 1.0 - 10.0 - 4.0 - self.footer_height()).max(0.0);
                content = content
                    .push(container(cosmic::widget::divider::horizontal::light()).padding([0, 12]))
                    .push(space::vertical().height(Length::Fixed(10.0)))
                    .push(container(body).max_height(room))
                    .push(space::vertical().height(Length::Fill));
                if self.shows_footer() {
                    content = content.push(self.footer());
                }
            }
        }
        let pill = mouse_area(container(content).width(Length::Fixed(w)).height(Length::Fixed(h)).clip(true)).on_enter(Message::PointerIn);
        // Centred in the surface (whatever size the compositor gave it yet), moved by the shake.
        let side = cosmic::iced::Padding { left: (2.0 * shake).max(0.0), right: (-2.0 * shake).max(0.0), ..Default::default() };
        let placed = column![space::vertical().height(Length::Fixed(MARGIN)), container(pill).padding(side).width(Length::Fill).align_x(cosmic::iced::Alignment::Center)];
        let ring = cosmic::iced::widget::canvas(self.aura()).width(Length::Fill).height(Length::Fill);
        let stack: Element<Message> = cosmic::iced::widget::stack![ring, placed].into();
        mouse_area(container(stack).width(Length::Fill).height(Length::Fill)).into()
    }
}

/// An argv as one line people can read (quoted where needed; it is never given to a shell).
pub fn shell_words(argv: &[String]) -> String {
    argv.iter()
        .map(|a| if !a.is_empty() && a.chars().all(|c| c.is_alphanumeric() || "-_./=:,@%+".contains(c)) { a.clone() } else { format!("'{}'", a.replace('\'', "'\\''")) })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The weather card: now, today's high and low, the next hours.
fn weather_view(w: &actions::weather::Weather) -> Element<'static, Message> {
    use actions::weather::{condition, degrees, icon};
    let icon_of = |code: i64, day: bool, size: u16| cosmic::widget::icon::from_name(icon(code, day)).size(size).icon();
    let mut details = vec![];
    if let Some(f) = w.feels_like {
        details.push(trf("Feels like {0}", &[&degrees(f)]));
    }
    if let Some(wind) = w.wind_kmh {
        details.push(trf("Wind {0} km/h", &[&format!("{}", wind.round())]));
    }
    let mut today = vec![];
    if let (Some(h), Some(l)) = (w.high, w.low) {
        today.push(trf("High {0} · low {1}", &[&degrees(h), &degrees(l)]));
    }
    if let Some(r) = w.rain_chance {
        today.push(trf("Rain {0} %", &[&r.to_string()]));
    }
    let mut place_lines = column![text::heading(w.place.name.clone())].spacing(0).align_x(cosmic::iced::Alignment::End);
    if !w.place.region.is_empty() {
        place_lines = place_lines.push(text::caption(w.place.region.clone()).class(theme::Text::Custom(dim_text)));
    }
    let now = row![
        icon_of(w.code, w.day, 40),
        column![
            text::title3(degrees(w.temperature)),
            text::body(tr(condition(w.code))),
        ]
        .spacing(0),
        space::horizontal().width(Length::Fill),
        column![
            place_lines,
            text::caption(details.join(" · ")).class(theme::Text::Custom(dim_text)),
            text::caption(today.join(" · ")).class(theme::Text::Custom(dim_text)),
        ]
        .spacing(2)
        .align_x(cosmic::iced::Alignment::End),
    ]
    .spacing(12)
    .align_y(cosmic::iced::Alignment::Center);
    let mut hours = row![].spacing(4);
    for h in &w.hours {
        hours = hours.push(
            column![text::caption(h.time.clone()).class(theme::Text::Custom(dim_text)), icon_of(h.code, h.day, 20), text::caption(degrees(h.temperature))]
                .spacing(2)
                .align_x(cosmic::iced::Alignment::Center)
                .width(Length::Fill),
        );
    }
    let mut c = column![now].spacing(10);
    if !w.hours.is_empty() {
        c = c.push(cosmic::widget::divider::horizontal::light()).push(hours);
    }
    container(c).padding(12).width(Length::Fill).class(theme::Container::custom(weather_box)).into()
}

/// An output's logical size: as xdg-output reports it, else its current mode over its scale.
fn logical_size(info: &cosmic::cctk::sctk::output::OutputInfo) -> Option<(f32, f32)> {
    if let Some((w, h)) = info.logical_size.filter(|(w, h)| *w > 0 && *h > 0) {
        return Some((w as f32, h as f32));
    }
    let mode = info.modes.iter().find(|m| m.current)?;
    let scale = info.scale_factor.max(1) as f32;
    Some((mode.dimensions.0 as f32 / scale, mode.dimensions.1 as f32 / scale))
}

/// Hyprland's focused monitor (where an overlay on the "active" output opens).
fn focused_hyprland_output() -> Option<String> {
    let out = std::process::Command::new("hyprctl").args(["-j", "monitors"]).stderr(std::process::Stdio::null()).output().ok()?;
    let list: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).ok()?;
    list.iter().find(|m| m.get("focused").and_then(serde_json::Value::as_bool) == Some(true))?.get("name")?.as_str().map(str::to_string)
}

/// The last `max` characters (live speech scrolls left in the compact field).
fn tail(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max { text.to_string() } else { format!("…{}", text.chars().skip(n - max + 1).collect::<String>()) }
}

/// How a block is set: the compact pill's size, or the reading view's larger type (`lead`: the
/// direct answer, the first paragraph, a little larger still).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Type {
    Compact,
    Reading { lead: bool },
}

impl Type {
    /// Text size and line height of paragraphs and list items.
    fn body(self) -> (f32, f32) {
        match self {
            Type::Compact => (14.5, 1.45),
            Type::Reading { lead: true } => (17.0, 1.4),
            Type::Reading { lead: false } => (15.0, 1.55),
        }
    }

    fn heading(self) -> f32 {
        if self == Type::Compact { 14.5 } else { 16.0 }
    }
}

/// A block's inline markup as rich text in the desktop's interface font (fonts.rs): bold,
/// italic, and inline code in the monospace font on a faint tint.
fn rich(raw: &str, size: f32, line: f32, weight: cosmic::iced::font::Weight) -> Element<'static, Message> {
    use cosmic::iced::font::{Style, Weight};
    use cosmic::iced::widget::{rich_text, span};
    let c = cosmic::theme::active();
    let mut tint = Color::from(c.cosmic().on_bg_color());
    tint.a = 0.09;
    let spans: Vec<cosmic::iced::widget::text::Span<'static, (), cosmic::font::Font>> = markdown::spans(raw)
        .into_iter()
        .map(|s| {
            if s.code {
                return span(s.text)
                    .font(cosmic::font::Font { weight: if s.bold { Weight::Bold } else { Weight::Normal }, ..cosmic::font::mono() })
                    .size(size * 0.88)
                    .background(tint)
                    .border(Border { radius: corners::now().small.into(), ..Default::default() })
                    .padding([0, 3]);
            }
            let mut font = fonts::prose_weight(if s.bold { Weight::Bold } else { weight });
            if s.italic {
                font.style = Style::Italic;
            }
            span(s.text).font(font)
        })
        .collect();
    rich_text(spans).size(size).line_height(cosmic::iced::widget::text::LineHeight::Relative(line)).width(Length::Fill).into()
}

fn block_view(block: &Block, kind: Type) -> Element<'static, Message> {
    use cosmic::iced::font::Weight;
    let (size, line) = kind.body();
    match block {
        Block::Heading(t) => rich(t, kind.heading(), 1.3, Weight::Semibold),
        Block::Paragraph(t) => rich(t, size, line, Weight::Normal),
        Block::Item(marker, t) => {
            // The marker in a fixed column: wrapped lines and every item's text start at one edge.
            let width = if kind == Type::Compact { 22.0 } else if marker.chars().count() > 2 { 30.0 } else { 24.0 };
            row![
                text::body(marker.clone()).size(size).line_height(cosmic::iced::widget::text::LineHeight::Relative(line)).font(fonts::prose()).class(theme::Text::Custom(dim_text)).width(Length::Fixed(width)),
                rich(t, size, line, Weight::Normal),
            ]
            .into()
        }
        Block::Code(t) => container(text::body(t.clone()).font(cosmic::font::mono()).size(if kind == Type::Compact { 12.5 } else { 13.5 })).padding(8).width(Length::Fill).class(theme::Container::custom(code_box)).into(),
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

fn accent_text(theme: &cosmic::Theme) -> iced_text::Style {
    iced_text::Style { color: Some(theme.cosmic().accent_text_color().into()), ..Default::default() }
}

fn warning_text(theme: &cosmic::Theme) -> iced_text::Style {
    iced_text::Style { color: Some(theme.cosmic().warning_color().into()), ..Default::default() }
}

fn chip_box(theme: &cosmic::Theme) -> iced_container::Style {
    let mut bg = Color::from(theme.cosmic().on_bg_color());
    bg.a = 0.06;
    iced_container::Style { background: Some(Background::Color(bg)), border: Border { radius: corners::now().control.into(), ..Default::default() }, ..Default::default() }
}

fn weather_box(theme: &cosmic::Theme) -> iced_container::Style {
    let mut bg = Color::from(theme.cosmic().on_bg_color());
    bg.a = 0.05;
    let mut border = Color::from(theme.cosmic().on_bg_color());
    border.a = 0.10;
    iced_container::Style { background: Some(Background::Color(bg)), border: Border { radius: corners::now().radius.into(), width: 1.0, color: border }, ..Default::default() }
}

/// Amber: the palette's warning colour (Maprisma Theme's yellow) as a tint and a border.
fn amber_card(theme: &cosmic::Theme) -> iced_container::Style {
    let warning = Color::from(theme.cosmic().warning_color());
    let mut bg = warning;
    bg.a = 0.12;
    iced_container::Style { background: Some(Background::Color(bg)), border: Border { radius: corners::now().radius.into(), width: 1.5, color: warning }, ..Default::default() }
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

/// Stops the helper's whole process group: SIGTERM now, SIGKILL after 2 s for whatever is still
/// in it (a descendant that ignores SIGTERM), on a thread of its own so the UI never waits.
fn kill_helper() {
    let group = HELPER_GROUP.swap(0, Ordering::SeqCst);
    if group > 0
        && let Ok(handle) = std::thread::Builder::new().name("helper-kill".into()).spawn(move || crate::procgroup::terminate(group, HELPER_GRACE))
    {
        HELPER_KILLS.lock().unwrap_or_else(|e| e.into_inner()).push(handle);
    }
}

static HELPER_KILLS: std::sync::Mutex<Vec<std::thread::JoinHandle<usize>>> = std::sync::Mutex::new(Vec::new());

/// When the overlay ends: the helper is stopped and every pending SIGKILL is carried out
/// before the process goes (else a TERM-ignoring descendant would survive the overlay).
pub fn finish_helper_kills() {
    kill_helper();
    let handles = std::mem::take(&mut *HELPER_KILLS.lock().unwrap_or_else(|e| e.into_inner()));
    for h in handles {
        let _ = h.join();
    }
}

const HELPER_GRACE: Duration = Duration::from_secs(2);

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
    /// Reading the answer aloud: a gentle pulse (the service sends no level for speech).
    Speaking,
}

struct AuraCanvas {
    /// The pill's size; it sits centred in the surface, `shake` px to the side, MARGIN from the top.
    size: Size,
    shake: f32,
    /// An auto-close counting down: the part left (a thin line along the pill's bottom).
    countdown: Option<f32>,
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
    fn gradient(&self, pill: Rectangle, colors: &AuraColors, angle: f32, alpha: f32) -> canvas::Style {
        let c = pill.center();
        let (dx, dy) = (angle.cos() * pill.width / 2.0, angle.sin() * pill.width / 2.0);
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
        let pill = Rectangle::new(Point::new((bounds.width - self.size.width) / 2.0 + self.shake, MARGIN), self.size);
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
            Mode::Speaking => {
                let pulse = if self.reduced { 0.5 } else { aura::speak_pulse(self.phase_t) };
                (1.8 + 0.8 * pulse, 0.18 + 0.30 * pulse, if self.reduced { 0.0 } else { self.t * 0.25 }, 0.8 + 0.2 * pulse)
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
                frame.stroke(&rounded(ring, r + f * 16.0), Stroke { style: self.gradient(pill, &colors, angle, a), width: 2.4, ..Default::default() });
            }
        }
        frame.fill(&rounded(pill, r), self.surface);
        // Transcribing: a ripple running inwards from the border.
        if self.mode == Mode::Transcribing && !self.reduced {
            let p = aura::ripple(self.phase_t);
            let inset = p * (pill.height / 2.0).min(14.0);
            let inner = grow(pill, -inset);
            frame.stroke(&rounded(inner, (r - inset).max(2.0)), Stroke { style: self.gradient(pill, &colors, 0.0, 0.55 * (1.0 - p)), width: 1.5, ..Default::default() });
        }
        // The ring.
        let half = width / 2.0;
        frame.stroke(&rounded(grow(pill, -half), (r - half).max(0.0)), Stroke { style: self.gradient(pill, &colors, angle, alpha), width, ..Default::default() });
        // An auto-close counting down: a thin line along the bottom, shrinking to the left.
        if let Some(left) = self.countdown {
            let inset = r.max(12.0);
            let full = (pill.width - 2.0 * inset).max(0.0);
            let y = pill.y + pill.height - 3.0;
            let line = Path::line(Point::new(pill.x + inset, y), Point::new(pill.x + inset + full * left, y));
            frame.stroke(&line, Stroke { style: canvas::Style::Solid(color(self.accent.primary, 0.55)), width: 2.0, line_cap: canvas::LineCap::Round, ..Default::default() });
        }
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
                    Mode::Speaking => (4.5 + 1.5 * if self.reduced { 0.5 } else { aura::speak_pulse(self.phase_t) }, 1.0),
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
            shown: String::new(),
            turn: Turn::default(),
            turn_gen: 0,
            tainted: false,
            speak_gen: 0,
            speak: None,
            speaking_since: None,
            view: View::Compact,
            outputs: vec![],
            reading: (answer::READING_WIDTH, answer::READING_FALLBACK_HEIGHT),
            surface_size: SURFACE,
            closing: None,
            close_gen: 0,
            close_kept: false,
        };
        fonts::load(look::desktop());
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
            cosmic::iced::event::listen_with(|event, status, id| match event {
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    use cosmic::iced::keyboard::{Key, key::Named};
                    if shot::is_shot_key(&key, modifiers) {
                        return Some(Message::ShotKey(id));
                    }
                    match key.as_ref() {
                        Key::Named(Named::Escape) => Some(Message::Escape),
                        Key::Character(c) if modifiers.control() && modifiers.shift() && c.eq_ignore_ascii_case("c") => Some(Message::CopyAnswer),
                        Key::Character(c) if modifiers.control() && !modifiers.shift() && c.eq_ignore_ascii_case("m") => Some(Message::ToggleMic),
                        Key::Character(c) if modifiers.control() && !modifiers.shift() && c.eq_ignore_ascii_case("z") => Some(Message::Undo(None)),
                        // Enter when the field doesn't have it (it would send Submit itself).
                        Key::Named(Named::Enter) if status == cosmic::iced::event::Status::Ignored => Some(Message::Submit),
                        // Any other key: the answer stays open (no auto-close).
                        _ => Some(Message::Activity),
                    }
                }
                cosmic::iced::Event::PlatformSpecific(cosmic::iced::event::PlatformSpecific::Wayland(WaylandEvent::Layer(event, _, id))) => {
                    // MAPRISMA_DICTATION_DEBUG=1: keyboard focus of the layer surfaces, for checking a compositor.
                    if *DEBUG {
                        eprintln!("maprisma-dictation: layer {id:?} {event:?}");
                    }
                    None
                }
                cosmic::iced::Event::PlatformSpecific(cosmic::iced::event::PlatformSpecific::Wayland(WaylandEvent::Output(OutputEvent::Created(Some(info)), output))) => {
                    Some(Message::Output(info.name.clone().unwrap_or_default(), logical_size(&info), output))
                }
                cosmic::iced::Event::PlatformSpecific(cosmic::iced::event::PlatformSpecific::Wayland(WaylandEvent::Output(OutputEvent::InfoUpdate(info), _))) => {
                    Some(Message::OutputInfo(info.name.clone().unwrap_or_default(), logical_size(&info)))
                }
                _ => None,
            }),
        ];
        if let Some(end) = self.voice_on {
            let key = (self.voice_gen, end);
            let session = if self.flags.demo { Subscription::run_with(key, voice::demo) } else { Subscription::run_with(key, voice::session) };
            subs.push(session.with(self.voice_gen).map(|(generation, event)| Message::Voice(generation, event)));
        }
        if let Some((text, language)) = &self.speak {
            let key = (self.speak_gen, text.clone(), language.clone());
            subs.push(Subscription::run_with(key, voice::speak).with(self.speak_gen).map(|(generation, event)| Message::Speak(generation, event)));
        }
        if self.closing.is_some_and(|c| c.undo) && self.reduced {
            // The footer's countdown (no line moves with reduced motion).
            subs.push(cosmic::iced::time::every(Duration::from_millis(250)).map(|_| Message::Frame));
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
        if *DEBUG && !matches!(message, Message::Frame | Message::LookTick | Message::Voice(_, VoiceEvent::Level(_))) {
            eprintln!("maprisma-dictation: {}", format!("{message:?}").chars().take(160).collect::<String>());
        }
        if matches!(message, Message::Activity | Message::PointerIn | Message::Input(_) | Message::Submit | Message::ToggleMic | Message::CopyAnswer | Message::Undo(_) | Message::Pick(..) | Message::ShotKey(_)) {
            self.keep_open();
        }
        match message {
            Message::Output(name, size, output) => {
                self.note_output(name.clone(), size);
                if self.surface.is_none() && self.output_wanted.as_deref() == Some(name.as_str()) {
                    return self.open(IcedOutput::Output(output));
                }
            }
            Message::OutputInfo(name, size) => self.note_output(name, size),
            Message::Activity | Message::PointerIn => {}
            Message::AutoClose(generation) => {
                if generation != self.close_gen || self.close_kept {
                    return Task::none();
                }
                let Some(closing) = self.closing.take() else { return Task::none() };
                let m = &self.machine;
                if m.phase != Phase::Done || m.confirming || !m.text.trim().is_empty() || self.voice_on.is_some() {
                    return Task::none();
                }
                if self.speak.is_some() || m.speaking {
                    // Let it finish reading the confirmation aloud first.
                    self.closing = Some(closing);
                    return Task::perform(tokio::time::sleep(Duration::from_millis(300)), move |_| cosmic::Action::App(Message::AutoClose(generation)));
                }
                return self.run(vec![Effect::Close]);
            }
            Message::OpenFallback => return self.open(IcedOutput::Active),
            Message::Command(Command::Toggle | Command::Close) => return self.run(vec![Effect::Close]),
            Message::Command(Command::Wake(rest)) => {
                let rest = Some(rest).filter(|r| !r.trim().is_empty());
                return self.handle(Event::Wake { rest });
            }
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
                if !self.shown.is_empty() {
                    let status = self.set_status(tr("Copied"));
                    return Task::batch([cosmic::iced::clipboard::write(self.shown.clone()), status]);
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
                let answer = result.as_ref().ok().cloned();
                let task = self.handle(Event::Answer(result));
                let Some(answer) = answer else { return task };
                let split = schema::split(&answer);
                self.shown = split.text.clone();
                self.turn_gen += 1;
                self.turn = if self.settings.actions && !self.flags.demo {
                    Turn::new(&self.machine.question, &split, self.tainted, self.settings.confirm_all, crate::last::now())
                } else {
                    Turn::default()
                };
                self.view = answer::classify(&self.shown, self.turn.items.len());
                if self.view == View::Reading {
                    self.reading = answer::reading_size(self.output_size(), (EXPANDED_WIDTH, MAX_HEIGHT));
                }
                self.body_height = None;
                if self.settings.speak_answers && !self.flags.demo && !self.shown.is_empty() {
                    self.speak_gen += 1;
                    self.speak = Some((self.shown.clone(), voice::speak_language(&ui_language(&self.settings.language)).to_string()));
                }
                // Window actions aim at the window that is active now (looked up first).
                let wants: Vec<Option<schema::Action>> = self.turn.items.iter().map(|i| i.action.clone()).collect();
                let next = if wants.iter().flatten().any(|a| matches!(a, schema::Action::CloseWindow | schema::Action::MoveWindow { .. })) {
                    let (desktop, generation) = (look::desktop(), self.turn_gen);
                    blocking(move || wants.iter().map(|a| a.as_ref().map(|a| exec::prepare(a, desktop)).unwrap_or_default()).collect(), move |targets| Message::Prepared(generation, targets))
                } else {
                    self.pump()
                };
                return Task::batch([task, next]);
            }
            Message::AskNow(question) => {
                let task = self.handle(Event::Typed(question));
                return Task::batch([task, self.handle(Event::Submit)]);
            }
            Message::Prepared(generation, targets) => {
                if generation == self.turn_gen {
                    for (item, target) in self.turn.items.iter_mut().zip(targets) {
                        item.target = target;
                    }
                    return self.pump();
                }
            }
            Message::ActionDone(generation, i, outcome) => {
                if generation == self.turn_gen {
                    // body_height stays: the sensor reports when the chip changes the size (the
                    // estimate knows no chips, and a same-size chip would leave it too small).
                    self.turn.finished(i, outcome, crate::last::now());
                    return self.pump();
                }
            }
            Message::ConfirmClicked => {
                if self.machine.confirming {
                    return self.run(vec![Effect::Confirm]);
                }
            }
            Message::CancelClicked => {
                if self.machine.confirming {
                    self.machine.confirming = false;
                    return self.run(vec![Effect::CancelConfirm]);
                }
            }
            Message::Undo(which) => {
                let i = match which {
                    Some(i) => Some(i),
                    None => self.turn.undoable().map(|(i, _)| i),
                };
                if let Some(i) = i {
                    if let Some(u) = self.turn.start_undo(i) {
                        let (ctx, generation) = (self.context(), self.turn_gen);
                        return blocking(move || exec::undo(&u, &ctx), move |result| Message::UndoDone(generation, i, result));
                    }
                }
            }
            Message::UndoDone(generation, i, result) => {
                if generation == self.turn_gen {
                    self.turn.undone(i, result, crate::last::now());
                    return self.pump();
                }
            }
            Message::Pick(i, choice) => {
                if let Some(app) = self.turn.pick(i, choice) {
                    let generation = self.turn_gen;
                    return blocking(move || exec::launch(&app), move |outcome| Message::ActionDone(generation, i, outcome));
                }
            }
            Message::Speak(generation, event) => {
                if generation != self.speak_gen || self.speak.is_none() {
                    return Task::none();
                }
                match event {
                    SpeakEvent::Speaking => {
                        self.speaking_since = Some(Instant::now());
                        return self.handle(Event::Speaking(true));
                    }
                    SpeakEvent::Ended(state, message) => {
                        self.speak = None;
                        self.speaking_since = None;
                        let task = self.handle(Event::Speaking(false));
                        if state == "error" && !message.is_empty() {
                            return Task::batch([task, self.set_status(message)]);
                        }
                        return task;
                    }
                    SpeakEvent::Failed(message) => {
                        self.speak = None;
                        let task = self.handle(Event::Speaking(false));
                        return Task::batch([task, self.set_status(message)]);
                    }
                }
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

    /// Esc/close stops the helper's whole group, also a descendant that ignores SIGTERM.
    #[test]
    fn helper_group_is_killed() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sh").args(["-c", "trap '' TERM; sleep 30 & wait"]).process_group(0).spawn().unwrap();
        let group = child.id() as i32;
        let until = Instant::now() + Duration::from_secs(3);
        while crate::procgroup::members(group).len() < 2 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(crate::procgroup::members(group).len(), 2);
        HELPER_GROUP.store(group, Ordering::SeqCst);
        let started = Instant::now();
        finish_helper_kills();
        assert!(started.elapsed() < HELPER_GRACE + Duration::from_secs(1));
        let _ = child.wait();
        let until = Instant::now() + Duration::from_secs(2);
        while !crate::procgroup::members(group).is_empty() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(crate::procgroup::members(group).is_empty());
        assert_eq!(HELPER_GROUP.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn heights_grow_with_the_answer_and_stop_at_the_maximum() {
        let short = estimate_height(&markdown::parse("Noon."), true, EXPANDED_WIDTH, MAX_HEIGHT, false);
        assert!(short > COMPACT.1 && short < 200.0, "{short}");
        let long = estimate_height(&markdown::parse(&"word ".repeat(600)), true, EXPANDED_WIDTH, MAX_HEIGHT, false);
        assert_eq!(long, MAX_HEIGHT);
        // The reading view: wider lines hold more, and it may grow taller.
        let text = markdown::parse(&"word ".repeat(150));
        let (compact, reading) = (estimate_height(&text, true, EXPANDED_WIDTH, 2000.0, false), estimate_height(&text, true, answer::READING_WIDTH, 2000.0, true));
        assert!(reading < compact, "{reading} < {compact}");
        assert_eq!(estimate_height(&markdown::parse(&"word ".repeat(2000)), true, answer::READING_WIDTH, 756.0, true), 756.0);
        assert_eq!(tail("abcdef", 4), "…def");
        assert_eq!(tail("abc", 4), "abc");
    }
}
