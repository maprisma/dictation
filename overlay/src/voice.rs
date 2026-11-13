//! One voice session with maprisma-voice (docs/protocol.md) as a stream of events for the UI.
//! Each session has its own bus connection: when the UI drops the stream (Esc, typing, closing),
//! the connection goes and the service cancels the session (`client_gone`) even if the explicit
//! `Cancel` didn't get out. `--demo` uses [demo] instead: the same events, made up.

use crate::state::End;
use futures_util::{Stream, StreamExt};
use maprisma_voice::client::{Signal, decode};
use maprisma_voice::dbus::{INTERFACE, NAME, PATH};
use maprisma_voice::i18n::{tr, trf};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use zbus::zvariant::Value;

pub const APP_ID: &str = "com.maprisma.Dictation";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Finish,
    Cancel,
}

/// The UI's handle on a running session.
#[derive(Clone)]
pub struct Controls(pub UnboundedSender<Control>);

impl std::fmt::Debug for Controls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Controls")
    }
}

#[derive(Debug, Clone)]
pub enum VoiceEvent {
    Ready(Controls),
    State(String),
    Partial(String),
    Final(String),
    Level(f64),
    /// `Error` before `Closed("error")`.
    Error(String),
    Closed(String),
    /// StartSession failed: why, for people.
    Failed(String),
}

fn method_error(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, _, _) if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" => {
            tr("The voice service is not running. Type your question.")
        }
        zbus::Error::MethodError(_, Some(text), _) => text.clone(),
        other => trf("Could not start: {0}", &[&other.to_string()]),
    }
}

/// A real session: `(generation, end)` identifies it (a new generation is a new session).
pub fn session(key: &(u64, End)) -> impl Stream<Item = VoiceEvent> + use<> {
    let end = key.1;
    cosmic::iced::stream::channel(64, async move |mut out: cosmic::iced::futures::channel::mpsc::Sender<VoiceEvent>| {
        use futures_util::SinkExt;
        let fail = |m: String| VoiceEvent::Failed(m);
        let connection = match zbus::Connection::session().await {
            Ok(c) => c,
            Err(e) => {
                let _ = out.send(fail(trf("Could not start: {0}", &[&e.to_string()]))).await;
                return;
            }
        };
        let rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(INTERFACE).unwrap().path(PATH).unwrap().build();
        let mut signals = match zbus::MessageStream::for_match_rule(rule, &connection, Some(1024)).await {
            Ok(s) => s,
            Err(e) => {
                let _ = out.send(fail(trf("Could not start: {0}", &[&e.to_string()]))).await;
                return;
            }
        };
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("end", (if end == End::Silence { "silence" } else { "release" }).into());
        let reply = connection.call_method(Some(NAME), PATH, Some(INTERFACE), "StartSession", &(APP_ID, "assistant", options)).await;
        let id: String = match reply.and_then(|r| r.body().deserialize::<String>()) {
            Ok(id) => id,
            Err(e) => {
                let _ = out.send(fail(method_error(&e))).await;
                return;
            }
        };
        let (control, mut controls) = unbounded_channel();
        let _ = out.send(VoiceEvent::Ready(Controls(control))).await;
        loop {
            tokio::select! {
                message = signals.next() => {
                    let Some(Ok(message)) = message else {
                        let _ = out.send(VoiceEvent::Closed("error".into())).await;
                        break;
                    };
                    let event = match decode(&message) {
                        Signal::State(s, state) if s == id => VoiceEvent::State(state),
                        Signal::Partial(s, text) if s == id => VoiceEvent::Partial(text),
                        Signal::Final(s, text, _typed, _) if s == id => VoiceEvent::Final(text),
                        Signal::Level(s, rms) if s == id => VoiceEvent::Level(rms),
                        Signal::Error(s, _, message) if s == id => VoiceEvent::Error(message),
                        Signal::Closed(s, reason) if s == id => {
                            let _ = out.send(VoiceEvent::Closed(reason)).await;
                            break;
                        }
                        _ => continue,
                    };
                    if out.send(event).await.is_err() {
                        break;
                    }
                }
                control = controls.recv() => {
                    let method = match control {
                        Some(Control::Finish) => "Finish",
                        Some(Control::Cancel) | None => "Cancel",
                    };
                    let _ = connection.call_method(Some(NAME), PATH, Some(INTERFACE), method, &(id.as_str(),)).await;
                    if method == "Cancel" {
                        break;
                    }
                }
            }
        }
        // Stay quiet until the UI drops the subscription.
        std::future::pending::<()>().await;
    })
}

/// The demo's question and answer (`--demo`).
pub const DEMO_QUESTION: &str = "What is the difference between a process and a thread?";
pub const DEMO_ANSWER: &str = "A **process** is a running program with its own memory; a **thread** is a path of execution inside a process.\n\n- Threads of one process share its memory and open files, so they talk cheaply but must lock shared data.\n- Processes are isolated: one crashing does not take the others down.\n\nOn Linux both are tasks; you can see the threads of a process with:\n\n```\nps -T -p <pid>\n```";

/// Speech-like levels for the demo: syllables of about 180 ms with pauses between words.
pub fn demo_level(t: f32) -> f64 {
    if !(0.25..3.0).contains(&t) {
        return 0.002;
    }
    let syllable = (t * std::f32::consts::PI / 0.18).sin().abs();
    let word = if (t * 1.3).fract() < 0.8 { 1.0 } else { 0.15 };
    (0.004 + 0.09 * syllable * word * (0.7 + 0.3 * (t * 7.3).sin().abs())) as f64
}

/// A made-up session: listening with levels and growing partial text for 3 s, then the text.
pub fn demo(key: &(u64, End)) -> impl Stream<Item = VoiceEvent> + use<> {
    let _ = key;
    cosmic::iced::stream::channel(64, async move |mut out: cosmic::iced::futures::channel::mpsc::Sender<VoiceEvent>| {
        use futures_util::SinkExt;
        let (control, mut controls) = unbounded_channel();
        let _ = out.send(VoiceEvent::Ready(Controls(control))).await;
        let _ = out.send(VoiceEvent::State("listening".into())).await;
        let words: Vec<&str> = DEMO_QUESTION.split(' ').collect();
        let started = std::time::Instant::now();
        let mut finish = false;
        loop {
            let t = started.elapsed().as_secs_f32();
            if t >= 3.2 || finish {
                break;
            }
            let _ = out.send(VoiceEvent::Level(demo_level(t))).await;
            let shown = ((t - 0.5) / 2.5 * words.len() as f32).clamp(0.0, words.len() as f32) as usize;
            if shown > 0 && (t * 20.0) as u32 % 16 == 0 {
                let _ = out.send(VoiceEvent::Partial(words[..shown].join(" "))).await;
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                c = controls.recv() => match c {
                    Some(Control::Finish) => finish = true,
                    _ => {
                        let _ = out.send(VoiceEvent::Closed("cancelled".into())).await;
                        std::future::pending::<()>().await;
                    }
                }
            }
        }
        let _ = out.send(VoiceEvent::State("transcribing".into())).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = out.send(VoiceEvent::Final(DEMO_QUESTION.into())).await;
        let _ = out.send(VoiceEvent::Closed("silence".into())).await;
        std::future::pending::<()>().await;
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn demo_levels_look_like_speech() {
        let levels: Vec<f32> = (0..64).map(|i| crate::aura::level_from_rms(super::demo_level(i as f32 * 0.05))).collect();
        assert!(levels[0] < 0.05 && levels[63] < 0.05);
        let speaking = &levels[6..58];
        assert!(speaking.iter().cloned().fold(0.0, f32::max) > 0.6);
        assert!(speaking.iter().any(|l| *l < 0.3), "pauses between syllables");
    }
}
