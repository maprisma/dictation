//! One overlay at a time: the open one owns `com.maprisma.Dictation` on the session bus. A second
//! `maprisma-dictation --toggle` asks it to `Toggle` (it closes) and exits; `--release` (the talk
//! key went up) tells it `Release`; `--wake` (the voice service heard the wake phrase) tells an
//! open overlay `Wake(rest)`: listen again, or ask `rest`. Calls arrive in the UI through [take_commands].

use std::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

pub const NAME: &str = "com.maprisma.Dictation";
pub const PATH: &str = "/com/maprisma/Dictation";
pub const INTERFACE: &str = "com.maprisma.Dictation";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Toggle,
    Release,
    Close,
    /// The wake phrase was heard while the overlay is open: listen (empty) or ask this.
    Wake(String),
}

static RECEIVER: Mutex<Option<UnboundedReceiver<Command>>> = Mutex::new(None);

struct Overlay {
    sender: UnboundedSender<Command>,
}

#[zbus::interface(name = "com.maprisma.Dictation")]
impl Overlay {
    /// Another start of `maprisma-dictation --toggle`: the open overlay closes.
    fn toggle(&self) {
        let _ = self.sender.send(Command::Toggle);
    }
    /// The talk key went up (hold-to-talk bindings).
    fn release(&self) {
        let _ = self.sender.send(Command::Release);
    }
    fn close(&self) {
        let _ = self.sender.send(Command::Close);
    }
    /// The wake phrase was heard (`maprisma-dictation --wake [--ask-rest REST]` while open).
    fn wake(&self, rest: String) {
        let _ = self.sender.send(Command::Wake(rest.chars().take(MAX_REST).collect()));
    }
}

/// The longest request a wake start passes on.
pub const MAX_REST: usize = 500;

/// Claims the name; None when another overlay has it.
pub async fn claim() -> zbus::Result<Option<zbus::Connection>> {
    let (sender, receiver) = unbounded_channel();
    let connection = zbus::connection::Builder::session()?.serve_at(PATH, Overlay { sender })?.build().await?;
    match connection.request_name_with_flags(NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into()).await {
        Ok(_) => {
            *RECEIVER.lock().unwrap_or_else(|e| e.into_inner()) = Some(receiver);
            Ok(Some(connection))
        }
        Err(zbus::Error::NameTaken) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The calls for the UI (once).
pub fn take_commands() -> Option<UnboundedReceiver<Command>> {
    RECEIVER.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Calls `method` on the open overlay; false when none answers.
pub async fn call(method: &str) -> bool {
    let Ok(connection) = zbus::Connection::session().await else { return false };
    connection.call_method(Some(NAME), PATH, Some(INTERFACE), method, &()).await.is_ok()
}

/// `--wake` while an overlay is open: it listens again (or asks `rest`) instead of closing.
pub async fn wake(rest: &str) -> bool {
    let Ok(connection) = zbus::Connection::session().await else { return false };
    connection.call_method(Some(NAME), PATH, Some(INTERFACE), "Wake", &(rest,)).await.is_ok()
}

/// `--release`: the overlay may still be starting (a quick tap), so try for up to 1.5 s.
pub async fn release() -> bool {
    let started = std::time::Instant::now();
    loop {
        if call("Release").await {
            return true;
        }
        if started.elapsed() > std::time::Duration::from_millis(1500) {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    }
}

/// Tests that own `com.maprisma.Dictation` on a private bus take turns.
#[cfg(test)]
pub static TEST_NAME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "needs a session bus: dbus-run-session -- cargo test -p maprisma-dictation -- --ignored"]
    async fn single_instance_and_toggle() {
        let _turn = TEST_NAME_LOCK.lock().await;
        let first = claim().await.unwrap().unwrap();
        let mut commands = take_commands().unwrap();
        assert!(claim().await.unwrap().is_none());
        assert!(call("Toggle").await);
        assert!(release().await);
        assert!(wake("open Firefox").await);
        assert!(wake("").await);
        assert_eq!(commands.recv().await, Some(Command::Toggle));
        assert_eq!(commands.recv().await, Some(Command::Release));
        assert_eq!(commands.recv().await, Some(Command::Wake("open Firefox".into())));
        assert_eq!(commands.recv().await, Some(Command::Wake(String::new())));
        drop(first);
    }
}
