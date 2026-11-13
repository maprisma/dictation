//! The CLI's side of the D-Bus API: `dictate` and `status`.

use crate::dbus::{INTERFACE, NAME, PATH};
use crate::i18n::{tr, trf};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use zbus::zvariant::{OwnedValue, Value};

pub struct DictateOptions {
    pub silence: bool,
    pub seconds: Option<f64>,
    pub language: Option<String>,
    pub mode: String,
    pub app_id: String,
}

/// One decoded signal of our interface.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    Partial(String, String),
    Final(String, String, bool, String),
    Level(String, f64),
    State(String, String),
    Error(String, String, String),
    Closed(String, String),
    Other,
}

pub fn decode(message: &zbus::Message) -> Signal {
    let header = message.header();
    let member = header.member().map(|m| m.as_str().to_string()).unwrap_or_default();
    let body = message.body();
    let parsed = match member.as_str() {
        "Partial" => body.deserialize::<(String, String)>().ok().map(|(a, b)| Signal::Partial(a, b)),
        "Final" => body.deserialize::<(String, String, HashMap<String, OwnedValue>)>().ok().map(|(a, b, d)| {
            let typed = d.get("typed").is_some_and(|v| matches!(&**v, Value::Bool(true)));
            let engine = d.get("engine").map(|v| match &**v { Value::Str(s) => s.to_string(), _ => String::new() }).unwrap_or_default();
            Signal::Final(a, b, typed, engine)
        }),
        "Level" => body.deserialize::<(String, f64)>().ok().map(|(a, b)| Signal::Level(a, b)),
        "State" => body.deserialize::<(String, String)>().ok().map(|(a, b)| Signal::State(a, b)),
        "Error" => body.deserialize::<(String, String, String)>().ok().map(|(a, b, c)| Signal::Error(a, b, c)),
        "Closed" => body.deserialize::<(String, String)>().ok().map(|(a, b)| Signal::Closed(a, b)),
        _ => None,
    };
    parsed.unwrap_or(Signal::Other)
}

fn session_of(s: &Signal) -> Option<&str> {
    match s {
        Signal::Partial(id, _) | Signal::Final(id, ..) | Signal::Level(id, _) | Signal::State(id, _) | Signal::Error(id, ..) | Signal::Closed(id, _) => Some(id),
        Signal::Other => None,
    }
}

fn dbus_message(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(_, Some(text), _) => text.clone(),
        other => other.to_string(),
    }
}

/// Exit codes: 0 text printed, 1 failed or cancelled, 3 no speech, 4 service missing.
pub async fn dictate(options: DictateOptions) -> i32 {
    let connection = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} ({e})", tr("The voice service is not running."));
            return 4;
        }
    };
    let rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(INTERFACE).unwrap().path(PATH).unwrap().build();
    let mut stream = match zbus::MessageStream::for_match_rule(rule, &connection, Some(1024)).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 4;
        }
    };
    let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
    opts.insert("end", (if options.silence { "silence" } else { "release" }).into());
    if let Some(l) = &options.language {
        opts.insert("language", l.as_str().into());
    }
    let reply = connection.call_method(Some(NAME), PATH, Some(INTERFACE), "StartSession", &(options.app_id.as_str(), options.mode.as_str(), opts)).await;
    let id: String = match reply.and_then(|r| r.body().deserialize::<String>()) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("{}", trf("Could not start: {0}", &[&dbus_message(&e)]));
            return if matches!(e, zbus::Error::MethodError(ref n, ..) if n.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown") { 4 } else { 1 };
        }
    };
    if options.silence {
        eprintln!("{}", tr("Listening… stop talking to finish."));
    } else if let Some(s) = options.seconds {
        eprintln!("{}", trf("Listening for {0} s…", &[&format!("{s}")]));
    } else {
        eprintln!("{}", tr("Listening… press Enter to finish, Ctrl+C to cancel."));
    }
    let finish_after = async {
        match options.seconds {
            Some(s) if !options.silence => tokio::time::sleep(Duration::from_secs_f64(s.max(0.1))).await,
            _ => std::future::pending::<()>().await,
        }
    };
    let mut finish_after = std::pin::pin!(finish_after);
    let mut finish_sent = false;
    let mut interrupt = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut stdin = tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(tokio::io::stdin()));
    let read_stdin = !options.silence && options.seconds.is_none();
    let mut stdin_open = read_stdin;
    let call = |method: &'static str| {
        let (connection, id) = (connection.clone(), id.clone());
        async move {
            let _ = connection.call_method(Some(NAME), PATH, Some(INTERFACE), method, &(id.as_str(),)).await;
        }
    };
    let mut final_text: Option<(String, bool, String)> = None;
    let mut error: Option<String> = None;
    loop {
        tokio::select! {
            message = stream.next() => {
                let Some(Ok(message)) = message else { return 1 };
                let signal = decode(&message);
                if session_of(&signal) != Some(id.as_str()) {
                    continue;
                }
                match signal {
                    Signal::Partial(_, text) => eprintln!("… {text}"),
                    Signal::Final(_, text, typed, engine) => final_text = Some((text, typed, engine)),
                    Signal::Error(_, _, message) => error = Some(message),
                    Signal::Closed(_, reason) => {
                        if let Some((text, typed, engine)) = final_text {
                            if reason == "preempted" {
                                eprintln!("{}", tr("Another app took over the microphone."));
                            }
                            if typed {
                                eprintln!("{}", trf("{0} typed the text.", &[&engine]));
                            } else {
                                println!("{text}");
                            }
                            return 0;
                        }
                        return match reason.as_str() {
                            "no_speech" => { eprintln!("{}", tr("No speech was heard.")); 3 }
                            "error" => { eprintln!("{}", trf("Speech recognition failed: {0}", &[error.as_deref().unwrap_or("")])); 1 }
                            _ => { eprintln!("{}", tr("Cancelled.")); 1 }
                        };
                    }
                    _ => {}
                }
            }
            line = stdin.next_line(), if stdin_open => {
                match line {
                    Ok(Some(_)) => { call("Finish").await; stdin_open = false; }
                    // Closed stdin (not a terminal): keep listening until Ctrl+C or the cap.
                    _ => stdin_open = false,
                }
            }
            _ = &mut finish_after, if !finish_sent => { call("Finish").await; finish_sent = true; }
            _ = interrupt.recv() => { call("Cancel").await; }
        }
    }
}

/// The daemon's `Status` as JSON (it is started by D-Bus activation when installed).
pub async fn status() -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let connection = zbus::Connection::session().await.map_err(|e| e.to_string())?;
    let reply = tokio::time::timeout(Duration::from_secs(10), connection.call_method(Some(NAME), PATH, Some(INTERFACE), "Status", &()))
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|e| dbus_message(&e))?;
    let map: HashMap<String, OwnedValue> = reply.body().deserialize().map_err(|e| e.to_string())?;
    Ok(map.iter().map(|(k, v)| (k.clone(), crate::dbus::value_to_json(v))).collect())
}
