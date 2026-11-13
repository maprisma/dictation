//! One overlay at a time: the open one owns `com.maprisma.Dictation` on the session bus. A second
//! `maprisma-dictation --toggle` asks it to `Toggle` (it closes) and exits; `--release` (the talk
//! key went up) tells it `Release`. Calls arrive in the UI through [take_commands].

use std::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

pub const NAME: &str = "com.maprisma.Dictation";
pub const PATH: &str = "/com/maprisma/Dictation";
pub const INTERFACE: &str = "com.maprisma.Dictation";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Toggle,
    Release,
    Close,
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
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "needs a session bus: dbus-run-session -- cargo test -p maprisma-dictation -- --ignored"]
    async fn single_instance_and_toggle() {
        let first = claim().await.unwrap().unwrap();
        let mut commands = take_commands().unwrap();
        assert!(claim().await.unwrap().is_none());
        assert!(call("Toggle").await);
        assert!(release().await);
        assert_eq!(commands.recv().await, Some(Command::Toggle));
        assert_eq!(commands.recv().await, Some(Command::Release));
        drop(first);
    }
}
