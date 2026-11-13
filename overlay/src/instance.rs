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
    /// A KWin script run for an action says what it did (actions/desktop.rs `kwin_script`).
    fn kwin_result(&self, token: String, result: String) {
        record_kwin_result(&token, &result);
    }
    /// The wake phrase was heard (`maprisma-dictation --wake [--ask-rest REST]` while open).
    fn wake(&self, rest: String) {
        let _ = self.sender.send(Command::Wake(rest.chars().take(MAX_REST).collect()));
    }
}

/// What KWin scripts reported, by their token (newest last, a few kept).
static KWIN_RESULTS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
static KWIN_ARRIVED: std::sync::Condvar = std::sync::Condvar::new();

pub fn record_kwin_result(token: &str, result: &str) {
    if token.is_empty() || token.len() > 64 {
        return;
    }
    let mut results = KWIN_RESULTS.lock().unwrap_or_else(|e| e.into_inner());
    results.retain(|(t, _)| t != token);
    if results.len() >= 16 {
        results.remove(0);
    }
    results.push((token.to_string(), result.chars().take(200).collect()));
    KWIN_ARRIVED.notify_all();
}

/// Waits (blocking) up to `timeout` for the script with this token to report.
pub fn wait_kwin_result(token: &str, timeout: std::time::Duration) -> Option<String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut results = KWIN_RESULTS.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(i) = results.iter().position(|(t, _)| t == token) {
            return Some(results.remove(i).1);
        }
        let left = deadline.checked_duration_since(std::time::Instant::now())?;
        results = KWIN_ARRIVED.wait_timeout(results, left).unwrap_or_else(|e| e.into_inner()).0;
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

    #[test]
    fn kwin_results_by_token() {
        let t = std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            record_kwin_result("tok-a", "ok");
        });
        assert_eq!(wait_kwin_result("tok-a", std::time::Duration::from_secs(2)).as_deref(), Some("ok"));
        t.join().unwrap();
        // taken once; another token or none at all times out
        assert_eq!(wait_kwin_result("tok-a", std::time::Duration::from_millis(30)), None);
        record_kwin_result("", "ok");
        assert_eq!(wait_kwin_result("", std::time::Duration::from_millis(10)), None);
    }

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
        // a KWin script's callDBus report reaches the waiting action
        let c = zbus::Connection::session().await.unwrap();
        c.call_method(Some(NAME), PATH, Some(INTERFACE), "KwinResult", &("maprisma_dictation_t_1", "no-window")).await.unwrap();
        let got = tokio::task::spawn_blocking(|| wait_kwin_result("maprisma_dictation_t_1", std::time::Duration::from_secs(2))).await.unwrap();
        assert_eq!(got.as_deref(), Some("no-window"));
        drop(first);
    }
}
