//! What the overlay is doing and what each event leads to — a pure state machine, so the
//! staged Esc, typing over speech, hold/tap and click-outside rules are tested without a screen.
//! ui.rs carries out the [Effect]s (D-Bus calls, the helper, closing).

/// Hold the key at least this long to talk (push-to-talk); shorter is a tap (toggle mode).
pub const HOLD_MS: u64 = 250;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Nothing running: the field waits for typing (or a mic restart with Ctrl+M).
    Idle,
    /// The voice service listens.
    Listening,
    /// The voice service turns the speech into text.
    Transcribing,
    /// The helper is answering.
    Thinking,
    /// An answer shows.
    Done,
    /// Something failed; the message shows.
    Error(String),
}

/// How a voice session ends (docs/protocol.md `end`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum End {
    Silence,
    Release,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The overlay opened: listen (with this end) or not.
    Opened { listen: Option<End> },
    /// The voice service's `State` signal.
    VoiceState(String),
    /// `Final` text.
    VoiceFinal(String),
    /// `Closed` without a `Final` first (no speech, cancelled, preempted, error…).
    VoiceClosed { reason: String, message: Option<String> },
    /// StartSession failed (service missing, not ready…).
    VoiceFailed(String),
    /// The field's text changed by typing.
    Typed(String),
    /// Enter.
    Submit,
    /// The helper answered (or failed).
    Answer(Result<String, String>),
    /// Esc.
    Escape,
    /// A click outside the pill.
    ClickOutside,
    /// The talk key went up (`--release`) after being held this long.
    Released { held_ms: u64 },
    /// Ctrl+M: listen again, or stop listening.
    ToggleMic,
    /// The voice service started (true) or stopped (false) reading the answer aloud.
    Speaking(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    StartVoice(End),
    FinishVoice,
    CancelVoice,
    /// Ask the helper this question.
    Ask(String),
    CancelAsk,
    /// Done: flash green.
    FlashDone,
    /// Error: shake and turn red.
    FlashError,
    Close,
    /// Enter on the amber card: run the action that waits.
    Confirm,
    /// Esc on the card, or a new question: the waiting action doesn't run.
    CancelConfirm,
    StopSpeaking,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub phase: Phase,
    /// The field's text (typed, or the final speech).
    pub text: String,
    /// The voice service's text so far (shown at 60 %).
    pub partial: String,
    /// The question being or last answered.
    pub question: String,
    pub answer: String,
    /// A short note under the field (no speech heard, another app took the mic…).
    pub note: Option<String>,
    /// A voice session is (being) started and not closed yet.
    pub voice: Option<End>,
    /// The talk key went up (the session is finishing).
    pub released: bool,
    /// An action waits for Enter on the amber card (ui.rs keeps this in step).
    pub confirming: bool,
    /// The answer is being read aloud.
    pub speaking: bool,
}

impl Default for Machine {
    fn default() -> Self {
        Machine { phase: Phase::Idle, text: String::new(), partial: String::new(), question: String::new(), answer: String::new(), note: None, voice: None, released: false, confirming: false, speaking: false }
    }
}

impl Machine {
    fn stop_speaking(&mut self) -> Vec<Effect> {
        if std::mem::take(&mut self.speaking) { vec![Effect::StopSpeaking] } else { vec![] }
    }

    /// A new question or session: the waiting card goes (a spoken "yes" never confirms).
    fn drop_card(&mut self) -> Vec<Effect> {
        let mut effects = self.stop_speaking();
        if std::mem::take(&mut self.confirming) {
            effects.push(Effect::CancelConfirm);
        }
        effects
    }

    fn start_voice(&mut self, end: End) -> Vec<Effect> {
        let mut effects = self.stop_speaking();
        effects.push(Effect::StartVoice(end));
        self.phase = Phase::Listening;
        self.voice = Some(end);
        self.partial.clear();
        self.note = None;
        self.released = false;
        effects
    }

    fn stop_voice(&mut self) -> Vec<Effect> {
        self.partial.clear();
        if self.voice.take().is_some() { vec![Effect::CancelVoice] } else { vec![] }
    }

    fn ask(&mut self, question: String) -> Vec<Effect> {
        let mut effects = self.drop_card();
        self.question = question.clone();
        self.answer.clear();
        self.text.clear();
        self.partial.clear();
        self.note = None;
        self.phase = Phase::Thinking;
        effects.push(Effect::Ask(question));
        effects
    }

    /// Clicking outside closes only while nothing would be lost.
    pub fn click_closes(&self) -> bool {
        if self.confirming {
            return false;
        }
        match self.phase {
            Phase::Done | Phase::Error(_) => self.text.trim().is_empty(),
            Phase::Idle => self.text.trim().is_empty(),
            Phase::Listening | Phase::Transcribing | Phase::Thinking => false,
        }
    }

    pub fn handle(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Opened { listen } => match listen {
                Some(end) => self.start_voice(end),
                None => vec![],
            },
            Event::VoiceState(state) if self.voice.is_some() => {
                match state.as_str() {
                    "listening" => self.phase = Phase::Listening,
                    "transcribing" => self.phase = Phase::Transcribing,
                    _ => {}
                }
                vec![]
            }
            Event::VoiceState(_) => vec![],
            Event::VoiceFinal(text) => {
                if self.voice.take().is_none() {
                    return vec![];
                }
                self.partial.clear();
                let text = text.trim().to_string();
                if text.is_empty() {
                    self.phase = Phase::Idle;
                    self.note = Some("No speech was heard.".into());
                    return vec![];
                }
                // Spoken questions go straight to the helper (tap: after the silence; hold: on release).
                self.ask(text)
            }
            Event::VoiceClosed { reason, message } => {
                if self.voice.take().is_none() {
                    return vec![];
                }
                self.partial.clear();
                match reason.as_str() {
                    "error" => {
                        self.phase = Phase::Error(message.unwrap_or_default());
                        vec![Effect::FlashError]
                    }
                    other => {
                        self.phase = if self.answer.is_empty() { Phase::Idle } else { Phase::Done };
                        self.note = match other {
                            "no_speech" => Some("No speech was heard.".into()),
                            "preempted" => Some("Another app took over the microphone.".into()),
                            _ => None,
                        };
                        vec![]
                    }
                }
            }
            Event::VoiceFailed(message) => {
                self.voice = None;
                self.partial.clear();
                // Typing still works: say why, and stay usable.
                self.phase = if self.answer.is_empty() { Phase::Idle } else { Phase::Done };
                self.note = Some(message);
                vec![]
            }
            Event::Typed(text) => {
                self.text = text;
                let mut effects = self.stop_voice();
                if matches!(self.phase, Phase::Listening | Phase::Transcribing | Phase::Error(_)) {
                    self.phase = if self.answer.is_empty() { Phase::Idle } else { Phase::Done };
                }
                self.note = None;
                effects.retain(|e| *e != Effect::FinishVoice);
                effects
            }
            Event::Submit => {
                let question = self.text.trim().to_string();
                if question.is_empty() && self.confirming && self.phase != Phase::Thinking {
                    // Enter on the card (only a key press gets here, never speech).
                    self.confirming = false;
                    return vec![Effect::Confirm];
                }
                if question.is_empty() || self.phase == Phase::Thinking {
                    return vec![];
                }
                let mut effects = self.stop_voice();
                effects.extend(self.ask(question));
                effects
            }
            Event::Answer(result) => {
                if self.phase != Phase::Thinking {
                    return vec![];
                }
                match result {
                    Ok(answer) => {
                        self.answer = answer;
                        self.phase = Phase::Done;
                        vec![Effect::FlashDone]
                    }
                    Err(message) => {
                        self.phase = Phase::Error(message);
                        vec![Effect::FlashError]
                    }
                }
            }
            Event::Speaking(on) => {
                self.speaking = on;
                vec![]
            }
            // Staged: stop the voice, the helper, the speech or the card first; the next Esc closes.
            Event::Escape if (self.speaking || self.confirming) && !matches!(self.phase, Phase::Listening | Phase::Transcribing | Phase::Thinking) => self.drop_card(),
            Event::Escape => match self.phase {
                Phase::Listening | Phase::Transcribing => {
                    let effects = self.stop_voice();
                    self.phase = if self.answer.is_empty() { Phase::Idle } else { Phase::Done };
                    effects
                }
                Phase::Thinking => {
                    self.phase = Phase::Idle;
                    // The question comes back into the field, to edit or send again.
                    self.text = self.question.clone();
                    vec![Effect::CancelAsk]
                }
                _ => {
                    let mut effects = self.stop_voice();
                    effects.push(Effect::Close);
                    effects
                }
            },
            Event::ClickOutside => {
                if self.click_closes() {
                    let mut effects = self.stop_voice();
                    effects.push(Effect::Close);
                    effects
                } else {
                    vec![]
                }
            }
            Event::Released { held_ms } => {
                if self.voice != Some(End::Release) || self.released {
                    return vec![];
                }
                if held_ms < HOLD_MS {
                    // A tap: keep listening, ending on silence instead.
                    self.voice = None;
                    let mut effects = vec![Effect::CancelVoice];
                    effects.extend(self.start_voice(End::Silence));
                    effects
                } else {
                    self.released = true;
                    self.phase = Phase::Transcribing;
                    vec![Effect::FinishVoice]
                }
            }
            Event::ToggleMic => match self.phase {
                Phase::Listening | Phase::Transcribing => {
                    // Stop listening and use what was heard.
                    if self.voice.is_some() {
                        self.phase = Phase::Transcribing;
                        vec![Effect::FinishVoice]
                    } else {
                        vec![]
                    }
                }
                Phase::Thinking => vec![],
                _ => {
                    self.text.clear();
                    self.start_voice(End::Silence)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened() -> Machine {
        let mut m = Machine::default();
        assert_eq!(m.handle(Event::Opened { listen: Some(End::Silence) }), vec![Effect::StartVoice(End::Silence)]);
        m
    }

    #[test]
    fn speak_then_answer() {
        let mut m = opened();
        assert_eq!(m.phase, Phase::Listening);
        m.handle(Event::VoiceState("transcribing".into()));
        assert_eq!(m.phase, Phase::Transcribing);
        assert_eq!(m.handle(Event::VoiceFinal(" What time is it? ".into())), vec![Effect::Ask("What time is it?".into())]);
        assert_eq!((m.phase.clone(), m.question.as_str(), m.voice), (Phase::Thinking, "What time is it?", None));
        // Closed after Final changes nothing.
        assert_eq!(m.handle(Event::VoiceClosed { reason: "silence".into(), message: None }), vec![]);
        assert_eq!(m.handle(Event::Answer(Ok("Noon.".into()))), vec![Effect::FlashDone]);
        assert_eq!((m.phase.clone(), m.answer.as_str()), (Phase::Done, "Noon."));
        assert!(m.click_closes());
        // A late answer after cancelling is ignored.
        assert_eq!(m.handle(Event::Answer(Ok("late".into()))), vec![]);
    }

    #[test]
    fn typing_cancels_the_voice_session() {
        let mut m = opened();
        m.partial = "hel".into();
        assert_eq!(m.handle(Event::Typed("o".into())), vec![Effect::CancelVoice]);
        assert_eq!((m.phase.clone(), m.voice, m.partial.as_str(), m.text.as_str()), (Phase::Idle, None, "", "o"));
        // More typing: nothing more to cancel; the closing signal is ignored.
        assert_eq!(m.handle(Event::Typed("op".into())), vec![]);
        assert_eq!(m.handle(Event::VoiceClosed { reason: "cancelled".into(), message: None }), vec![]);
        assert_eq!(m.handle(Event::VoiceFinal("ignored".into())), vec![]);
        assert!(!m.click_closes(), "typed text would be lost");
        assert_eq!(m.handle(Event::Submit), vec![Effect::Ask("op".into())]);
        assert_eq!(m.handle(Event::Submit), vec![], "no second ask while thinking");
    }

    #[test]
    fn staged_escape() {
        // Listening: the first Esc stops the voice, the second closes.
        let mut m = opened();
        assert_eq!(m.handle(Event::Escape), vec![Effect::CancelVoice]);
        assert_eq!(m.phase, Phase::Idle);
        assert_eq!(m.handle(Event::Escape), vec![Effect::Close]);
        // Thinking: the first Esc cancels the helper and gives the question back.
        let mut m = Machine::default();
        m.handle(Event::Typed("why".into()));
        m.handle(Event::Submit);
        assert_eq!(m.handle(Event::Escape), vec![Effect::CancelAsk]);
        assert_eq!((m.phase.clone(), m.text.as_str()), (Phase::Idle, "why"));
        assert_eq!(m.handle(Event::Answer(Ok("late".into()))), vec![], "a cancelled answer never shows");
        assert_eq!(m.handle(Event::Escape), vec![Effect::Close]);
        // An answer showing: Esc closes.
        let mut m = Machine::default();
        m.handle(Event::Typed("q".into()));
        m.handle(Event::Submit);
        m.handle(Event::Answer(Ok("a".into())));
        assert_eq!(m.handle(Event::Escape), vec![Effect::Close]);
        // A follow-up being spoken over an answer: the first Esc stops it, the answer stays.
        assert_eq!(m.handle(Event::ToggleMic), vec![Effect::StartVoice(End::Silence)]);
        assert_eq!(m.handle(Event::Escape), vec![Effect::CancelVoice]);
        assert_eq!((m.phase.clone(), m.answer.as_str()), (Phase::Done, "a"));
    }

    #[test]
    fn click_outside_only_when_nothing_is_lost() {
        let mut m = opened();
        assert_eq!(m.handle(Event::ClickOutside), vec![], "listening");
        m.handle(Event::VoiceFinal("q".into()));
        assert_eq!(m.handle(Event::ClickOutside), vec![], "thinking");
        m.handle(Event::Answer(Err("boom".into())));
        assert_eq!(m.phase, Phase::Error("boom".into()));
        assert_eq!(m.handle(Event::ClickOutside), vec![Effect::Close]);
        let mut m = Machine::default();
        assert_eq!(m.handle(Event::ClickOutside), vec![Effect::Close], "idle, empty field");
    }

    #[test]
    fn hold_and_tap() {
        // Held long enough: release finishes.
        let mut m = Machine::default();
        assert_eq!(m.handle(Event::Opened { listen: Some(End::Release) }), vec![Effect::StartVoice(End::Release)]);
        assert_eq!(m.handle(Event::Released { held_ms: 900 }), vec![Effect::FinishVoice]);
        assert_eq!(m.phase, Phase::Transcribing);
        assert_eq!(m.handle(Event::Released { held_ms: 900 }), vec![], "only once");
        // A tap: the session restarts, ending on silence.
        let mut m = Machine::default();
        m.handle(Event::Opened { listen: Some(End::Release) });
        assert_eq!(m.handle(Event::Released { held_ms: 120 }), vec![Effect::CancelVoice, Effect::StartVoice(End::Silence)]);
        assert_eq!((m.phase.clone(), m.voice), (Phase::Listening, Some(End::Silence)));
        // Release in silence mode means nothing.
        assert_eq!(m.handle(Event::Released { held_ms: 900 }), vec![]);
    }

    #[test]
    fn voice_problems_leave_typing_usable() {
        let mut m = opened();
        assert_eq!(m.handle(Event::VoiceFailed("The voice service is not running.".into())), vec![]);
        assert_eq!((m.phase.clone(), m.voice), (Phase::Idle, None));
        assert!(m.note.is_some());
        let mut m = opened();
        m.handle(Event::VoiceClosed { reason: "no_speech".into(), message: None });
        assert_eq!((m.phase.clone(), m.note.as_deref()), (Phase::Idle, Some("No speech was heard.")));
        let mut m = opened();
        assert_eq!(m.handle(Event::VoiceClosed { reason: "error".into(), message: Some("engine".into()) }), vec![Effect::FlashError]);
        assert_eq!(m.phase, Phase::Error("engine".into()));
        // Typing after an error goes on normally.
        m.handle(Event::Typed("x".into()));
        assert_eq!(m.phase, Phase::Idle);
        let mut m = opened();
        m.handle(Event::VoiceFinal("   ".into()));
        assert_eq!((m.phase.clone(), m.note.as_deref()), (Phase::Idle, Some("No speech was heard.")));
    }

    #[test]
    fn ctrl_m_toggles_the_microphone() {
        let mut m = Machine::default();
        assert_eq!(m.handle(Event::ToggleMic), vec![Effect::StartVoice(End::Silence)]);
        assert_eq!(m.handle(Event::ToggleMic), vec![Effect::FinishVoice]);
        assert_eq!(m.phase, Phase::Transcribing);
        m.handle(Event::VoiceFinal("q".into()));
        assert_eq!(m.handle(Event::ToggleMic), vec![], "not while thinking");
    }

    fn answered() -> Machine {
        let mut m = opened();
        m.handle(Event::VoiceFinal("open firefox".into()));
        m.handle(Event::Answer(Ok("Opening.".into())));
        m
    }

    #[test]
    fn the_confirmation_card() {
        let mut m = answered();
        m.confirming = true;
        assert!(!m.click_closes(), "a waiting card is not lost by a click");
        // Enter with an empty field runs it.
        assert_eq!(m.handle(Event::Submit), vec![Effect::Confirm]);
        assert!(!m.confirming);
        // Esc cancels the card first; the next Esc closes.
        m.confirming = true;
        assert_eq!(m.handle(Event::Escape), vec![Effect::CancelConfirm]);
        assert_eq!(m.handle(Event::Escape), vec![Effect::Close]);
        // A typed question instead: the card goes, the question is asked.
        let mut m = answered();
        m.confirming = true;
        m.handle(Event::Typed("no, open chrome".into()));
        assert_eq!(m.handle(Event::Submit), vec![Effect::CancelConfirm, Effect::Ask("no, open chrome".into())]);
        // Speech never confirms: a spoken "yes" is a new question and the card goes.
        let mut m = answered();
        m.confirming = true;
        m.handle(Event::ToggleMic);
        assert_eq!(m.handle(Event::VoiceFinal("yes".into())), vec![Effect::CancelConfirm, Effect::Ask("yes".into())]);
        assert!(!m.confirming);
    }

    #[test]
    fn speaking_answers() {
        let mut m = answered();
        m.handle(Event::Speaking(true));
        // Esc stops the voice first, then closes.
        assert_eq!(m.handle(Event::Escape), vec![Effect::StopSpeaking]);
        assert_eq!(m.handle(Event::Escape), vec![Effect::Close]);
        // A new voice session stops it too.
        let mut m = answered();
        m.handle(Event::Speaking(true));
        assert_eq!(m.handle(Event::ToggleMic), vec![Effect::StopSpeaking, Effect::StartVoice(End::Silence)]);
        // Done speaking: Esc closes at once.
        let mut m = answered();
        m.handle(Event::Speaking(true));
        m.handle(Event::Speaking(false));
        assert_eq!(m.handle(Event::Escape), vec![Effect::Close]);
    }
}
