//! `maprisma-actions` (docs/overlay.md "Actions"): the helper never gets a shell or tools; it may
//! propose actions in one fenced ```maprisma-actions block of a fixed schema (schema.rs). Each
//! gets a level (policy.rs) and shows as a chip; level 0 runs at once, level 1 waits for Enter
//! on an amber card, level 2 never runs. [Turn] is the bookkeeping of one answer's actions — in
//! order, one at a time, with Undo — kept free of the UI so it is tested on its own.

pub mod apps;
pub mod desktop;
pub mod exec;
pub mod log;
pub mod policy;
pub mod schema;
pub mod system;
pub mod weather;

use exec::{Outcome, Target, Undo};
use maprisma_voice::i18n::{tr, trf};
use policy::Level;
use schema::{Action, Media, Split, ThemeMode};

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Behind an earlier action (or a confirmation) in this answer.
    Waiting,
    /// The amber card shows: Enter runs it, Esc cancels.
    Confirm,
    Running,
    Done { text: String, undo: Option<Undo> },
    Undone(String),
    Weather(Box<weather::Weather>),
    Output { ok: bool, text: String, output: String },
    Choices(Vec<apps::App>),
    Failed(String),
    Unsupported(String),
    /// Level 2: why.
    Blocked(String),
    Cancelled,
    /// The helper's object didn't fit the schema.
    Rejected(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub action: Option<Action>,
    pub level: Level,
    pub target: Target,
    pub state: State,
}

/// What the UI does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Run(usize),
    Confirm(usize),
    Idle,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Turn {
    pub request: String,
    pub items: Vec<Item>,
    /// Log lines not written yet (the UI drains them).
    pub log: Vec<log::Entry>,
}

/// What the chip or card says the action is.
pub fn describe(action: &Action) -> String {
    match action {
        Action::OpenApp { name } => trf("Open {0}", &[name]),
        Action::OpenUrl { url } => trf("Open {0}", &[&exec::host_of(url)]),
        Action::Dnd { on: true } => tr("Do not disturb on"),
        Action::Dnd { on: false } => tr("Do not disturb off"),
        Action::ThemeMode { mode: ThemeMode::Light } => tr("Light theme"),
        Action::ThemeMode { mode: ThemeMode::Dark } => tr("Dark theme"),
        Action::Volume { mute: Some(true), percent: None, change: None } => tr("Mute"),
        Action::Volume { mute: Some(false), percent: None, change: None } => tr("Unmute"),
        Action::Volume { percent: Some(p), .. } => trf("Volume {0} %", &[&p.to_string()]),
        Action::Volume { change: Some(c), .. } => trf("Volume {0} %", &[&format!("{c:+}")]),
        Action::Volume { .. } => tr("Volume"),
        Action::Media { command: Media::PlayPause } => tr("Play/pause"),
        Action::Media { command: Media::Next } => tr("Next track"),
        Action::Media { command: Media::Previous } => tr("Previous track"),
        Action::Workspace { number } => trf("Workspace {0}", &[&number.to_string()]),
        Action::MoveWindow { workspace } => trf("Move the window to workspace {0}", &[&workspace.to_string()]),
        Action::Weather { location } if location.is_empty() => tr("Weather"),
        Action::Weather { location } => trf("Weather in {0}", &[location]),
        Action::CloseWindow => tr("Close the window"),
        Action::RunCommand { .. } => tr("Run a command"),
    }
}

impl Turn {
    /// One answer's actions. `tainted`: outside text is already in this turn (policy.rs).
    pub fn new(request: &str, split: &Split, tainted: bool, confirm_all: bool, now: u64) -> Turn {
        let mut turn = Turn { request: request.to_string(), ..Default::default() };
        let good: Vec<Action> = split.actions.iter().filter_map(|a| a.as_ref().ok().cloned()).collect();
        let mut levels = policy::classify(&good, tainted, confirm_all).into_iter();
        if let Some(why) = &split.rejected {
            turn.items.push(Item { action: None, level: Level::Blocked(why.clone()), target: Target::default(), state: State::Rejected(why.clone()) });
        }
        for a in &split.actions {
            let item = match a {
                Ok(action) => {
                    let level = levels.next().unwrap_or(Level::Confirm);
                    let state = match &level {
                        Level::Blocked(why) => State::Blocked(why.clone()),
                        _ => State::Waiting,
                    };
                    Item { action: Some(action.clone()), level, target: Target::default(), state }
                }
                Err(why) => Item { action: None, level: Level::Blocked(why.clone()), target: Target::default(), state: State::Rejected(why.clone()) },
            };
            turn.items.push(item);
        }
        for i in 0..turn.items.len() {
            match turn.items[i].state.clone() {
                State::Blocked(why) => turn.note(i, "blocked", &why, now),
                State::Rejected(why) => turn.note(i, "rejected", &why, now),
                _ => {}
            }
        }
        turn
    }

    fn note(&mut self, i: usize, result: &'static str, detail: &str, now: u64) {
        let item = &self.items[i];
        self.log.push(log::Entry {
            time: now,
            request: self.request.clone(),
            action: item.action.as_ref().map(Action::to_json).unwrap_or(serde_json::Value::Null),
            level: item.level.number(),
            result,
            detail: detail.to_string(),
        });
    }

    /// Starts the next waiting action: runs a level 0 one, or shows the card for a level 1 one.
    /// Nothing starts while one runs or waits for Enter.
    pub fn advance(&mut self) -> Next {
        if self.items.iter().any(|i| matches!(i.state, State::Running | State::Confirm)) {
            return Next::Idle;
        }
        let Some(i) = self.items.iter().position(|i| i.state == State::Waiting) else { return Next::Idle };
        if self.items[i].level == Level::Run {
            self.items[i].state = State::Running;
            Next::Run(i)
        } else {
            self.items[i].state = State::Confirm;
            Next::Confirm(i)
        }
    }

    pub fn confirming(&self) -> Option<usize> {
        self.items.iter().position(|i| i.state == State::Confirm)
    }

    /// Enter on the card.
    pub fn confirm(&mut self) -> Option<usize> {
        let i = self.confirming()?;
        self.items[i].state = State::Running;
        Some(i)
    }

    /// Esc on the card: this one and the ones after it don't run.
    pub fn cancel(&mut self, now: u64) -> bool {
        let Some(i) = self.confirming() else { return false };
        for j in i..self.items.len() {
            if matches!(self.items[j].state, State::Confirm | State::Waiting) {
                self.items[j].state = State::Cancelled;
                self.note(j, "cancelled", "", now);
            }
        }
        true
    }

    /// A new question (typed or spoken) or closing: nothing left over runs.
    pub fn cancel_all(&mut self, now: u64) {
        for j in 0..self.items.len() {
            if matches!(self.items[j].state, State::Confirm | State::Waiting | State::Choices(_)) {
                self.items[j].state = State::Cancelled;
                self.note(j, "cancelled", "", now);
            }
        }
    }

    pub fn finished(&mut self, i: usize, outcome: Outcome, now: u64) {
        let Some(item) = self.items.get_mut(i) else { return };
        if item.state != State::Running {
            return;
        }
        let (state, result, detail) = match outcome {
            Outcome::Done { text, undo } => (State::Done { text: text.clone(), undo }, "done", text),
            Outcome::Weather(w) => {
                let s = weather::summary(&w);
                (State::Weather(w), "done", s)
            }
            Outcome::Output { ok, text, output } => {
                let detail = format!("{text}: {}", output.chars().take(300).collect::<String>());
                (State::Output { ok, text, output }, if ok { "done" } else { "failed" }, detail)
            }
            Outcome::Choices(list) => {
                // Not logged yet: the pick is.
                item.state = State::Choices(list);
                return;
            }
            Outcome::Failed(e) => (State::Failed(e.clone()), "failed", e),
            Outcome::Unsupported(e) => (State::Unsupported(e.clone()), "unsupported", e),
        };
        item.state = state;
        self.note(i, result, &detail, now);
    }

    /// A choice of several apps was picked: it runs.
    pub fn pick(&mut self, i: usize, choice: usize) -> Option<apps::App> {
        let item = self.items.get_mut(i)?;
        let State::Choices(list) = &item.state else { return None };
        let app = list.get(choice)?.clone();
        item.state = State::Running;
        Some(app)
    }

    /// The newest action that can be undone.
    pub fn undoable(&self) -> Option<(usize, Undo)> {
        self.items.iter().enumerate().rev().find_map(|(i, item)| match &item.state {
            State::Done { undo: Some(u), .. } => Some((i, u.clone())),
            _ => None,
        })
    }

    /// Undo was asked for (Running until it is back).
    pub fn start_undo(&mut self, i: usize) -> Option<Undo> {
        let item = self.items.get_mut(i)?;
        let State::Done { undo: Some(u), text } = &item.state else { return None };
        let u = u.clone();
        item.state = State::Undone(text.clone());
        Some(u)
    }

    pub fn undone(&mut self, i: usize, result: Result<(), String>, now: u64) {
        match result {
            Ok(()) => self.note(i, "undone", "", now),
            Err(e) => {
                if let Some(item) = self.items.get_mut(i) {
                    item.state = State::Failed(trf("Undo did not work: {0}", &[&e]));
                }
                self.note(i, "failed", &format!("undo: {e}"), now);
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.items.iter().any(|i| i.state == State::Running)
    }

    /// Outside text came in (a weather result): a follow-up turn needs Enter for its actions.
    pub fn read_outside(&self) -> bool {
        self.items.iter().any(|i| matches!(i.state, State::Weather(_)))
    }

    /// What happened, in English, for the follow-up's context (the helper sees its results).
    pub fn context(&self) -> String {
        let mut lines = vec![];
        for item in &self.items {
            let what = item.action.as_ref().map(|a| a.kind()).unwrap_or("invalid");
            let line = match &item.state {
                State::Weather(w) => weather::summary(w),
                State::Done { text, .. } => format!("{what}: done ({text})"),
                State::Undone(_) => format!("{what}: done, then undone by the user"),
                State::Output { ok, output, .. } => format!("{what}: {} — output: {}", if *ok { "finished" } else { "failed" }, output.chars().take(1500).collect::<String>()),
                State::Failed(e) | State::Unsupported(e) => format!("{what}: did not work ({e})"),
                State::Blocked(why) | State::Rejected(why) => format!("{what}: refused ({why})"),
                State::Cancelled => format!("{what}: cancelled by the user"),
                State::Choices(list) => format!("{what}: several apps fit ({})", list.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")),
                State::Waiting | State::Confirm | State::Running => continue,
            };
            lines.push(format!("- {line}"));
        }
        if lines.is_empty() { String::new() } else { format!("[Actions]\n{}", lines.join("\n")) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(json: &str) -> Split {
        schema::split(&format!("Sure.\n```maprisma-actions\n{json}\n```"))
    }

    #[test]
    fn one_at_a_time_with_a_confirmation() {
        let s = split(r#"[{"kind": "open_app", "name": "Firefox"}, {"kind": "run_command", "argv": ["uname", "-r"], "why": "kernel"}, {"kind": "dnd", "on": true}, {"kind": "run_command", "argv": ["sudo", "x"], "why": ""}, {"kind": "bogus"}]"#);
        let mut t = Turn::new("open firefox and show the kernel", &s, false, false, 1);
        assert_eq!(t.items.len(), 5);
        assert!(matches!(t.items[3].state, State::Blocked(_)));
        assert!(matches!(t.items[4].state, State::Rejected(_)));
        assert_eq!(t.log.iter().map(|e| e.result).collect::<Vec<_>>(), ["blocked", "rejected"]);
        assert_eq!(t.log[0].level, 2);
        assert_eq!(t.advance(), Next::Run(0));
        assert_eq!(t.advance(), Next::Idle, "one at a time");
        t.finished(0, Outcome::Done { text: "Opened Firefox".into(), undo: None }, 2);
        assert_eq!(t.advance(), Next::Confirm(1));
        assert_eq!(t.confirming(), Some(1));
        assert_eq!(t.advance(), Next::Idle, "waits for Enter");
        assert_eq!(t.confirm(), Some(1));
        assert_eq!(t.items[1].state, State::Running);
        t.finished(1, Outcome::Output { ok: true, text: "Command finished".into(), output: "7.4.0".into() }, 3);
        assert_eq!(t.advance(), Next::Run(2));
        t.finished(2, Outcome::Done { text: "Do not disturb is on".into(), undo: Some(Undo::Dnd(false)) }, 4);
        assert_eq!(t.advance(), Next::Idle);
        assert_eq!(t.log.iter().map(|e| e.result).collect::<Vec<_>>(), ["blocked", "rejected", "done", "done", "done"]);
        assert_eq!(t.log[3].level, 1);
        assert!(t.context().contains("run_command: finished — output: 7.4.0"));
    }

    #[test]
    fn cancelling_and_new_questions() {
        let s = split(r#"[{"kind": "close_window"}, {"kind": "workspace", "number": 2}]"#);
        let mut t = Turn::new("q", &s, false, false, 1);
        assert_eq!(t.advance(), Next::Confirm(0));
        assert!(t.cancel(2));
        assert_eq!(t.items[0].state, State::Cancelled);
        assert_eq!(t.items[1].state, State::Cancelled, "the rest goes too");
        assert_eq!(t.advance(), Next::Idle);
        assert!(!t.cancel(3));
        let mut t = Turn::new("q", &s, false, false, 1);
        t.advance();
        t.cancel_all(2);
        assert!(t.items.iter().all(|i| i.state == State::Cancelled));
        assert_eq!(t.log.len(), 2);
        // a tainted turn: the workspace switch waits for Enter too
        let mut t = Turn::new("q", &split(r#"[{"kind": "workspace", "number": 2}]"#), true, false, 1);
        assert_eq!(t.advance(), Next::Confirm(0));
    }

    #[test]
    fn undo_bookkeeping() {
        let s = split(r#"[{"kind": "volume", "change": 10}, {"kind": "theme_mode", "mode": "dark"}, {"kind": "media", "command": "next"}]"#);
        let mut t = Turn::new("q", &s, false, false, 1);
        assert_eq!(t.advance(), Next::Run(0));
        t.finished(0, Outcome::Done { text: "Volume 50 %".into(), undo: Some(Undo::Volume(0.4, false)) }, 2);
        assert_eq!(t.advance(), Next::Run(1));
        t.finished(1, Outcome::Done { text: "Dark theme: nord".into(), undo: Some(Undo::Theme("white".into())) }, 2);
        assert_eq!(t.advance(), Next::Run(2));
        t.finished(2, Outcome::Done { text: "Next track".into(), undo: None }, 2);
        // the newest undoable first
        assert_eq!(t.undoable(), Some((1, Undo::Theme("white".into()))));
        assert_eq!(t.start_undo(1), Some(Undo::Theme("white".into())));
        assert_eq!(t.start_undo(1), None, "only once");
        t.undone(1, Ok(()), 3);
        assert_eq!(t.undoable(), Some((0, Undo::Volume(0.4, false))));
        t.start_undo(0);
        t.undone(0, Err("wpctl failed".into()), 4);
        assert!(matches!(&t.items[0].state, State::Failed(e) if e.contains("wpctl failed")));
        assert_eq!(t.undoable(), None);
        assert_eq!(t.log.iter().filter(|e| e.result == "undone").count(), 1);
        assert!(t.context().contains("theme_mode: done, then undone by the user"));
    }

    #[test]
    fn choices_and_weather() {
        let s = split(r#"[{"kind": "open_app", "name": "browser"}, {"kind": "weather", "location": "Berlin"}]"#);
        let mut t = Turn::new("q", &s, false, false, 1);
        assert_eq!(t.advance(), Next::Run(0));
        let app = |n: &str| apps::App { id: format!("{n}.desktop"), path: n.into(), name: n.into(), generic: String::new(), keywords: vec![], icon: String::new(), standard: true };
        t.finished(0, Outcome::Choices(vec![app("Firefox"), app("Chrome")]), 2);
        assert!(t.log.is_empty(), "the pick is logged, not the question");
        // the next action may go on while the choice is open
        assert_eq!(t.advance(), Next::Run(1));
        assert_eq!(t.pick(0, 1).map(|a| a.name), Some("Chrome".into()));
        assert_eq!(t.pick(0, 0), None);
        t.finished(0, Outcome::Done { text: "Opened Chrome".into(), undo: None }, 3);
        assert!(!t.read_outside());
        let place = weather::Place { name: "Berlin".into(), region: String::new(), latitude: 0.0, longitude: 0.0 };
        let w = weather::Weather { place, temperature: 8.0, feels_like: None, code: 3, day: true, wind_kmh: None, high: None, low: None, rain_chance: None, hours: vec![] };
        t.finished(1, Outcome::Weather(Box::new(w)), 3);
        assert!(t.read_outside());
        assert!(t.context().contains("Weather in Berlin: 8 °C, overcast"));
        assert_eq!(describe(&Action::Volume { percent: None, mute: None, change: Some(-5) }), "Volume -5 %");
    }
}
