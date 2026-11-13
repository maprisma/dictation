//! Omarchy's dictation keys through this service (docs/omarchy.md): Omarchy runs
//! `omarchy-dictation-maprisma start|stop|toggle`, which calls `maprisma-voice omarchy …`.
//!
//! - `start` starts a detached `maprisma-voice omarchy-run` and returns at once. That process
//!   holds one voice session (end = release) and types the final text into the focused window
//!   with `wtype` (Omarchy installs it), else puts it on the clipboard (`wl-copy`).
//! - `stop` sends it SIGUSR1: it finishes the session (transcribes) and exits. Without a running
//!   one, `stop` succeeds (Omarchy's rule: stop must succeed when idle).
//! - `toggle` is `stop` when one runs, else `start`.
//!
//! The running one is found by a pid file in our private runtime folder, checked against the
//! process's own command line, so a reused pid is never signalled.

use crate::dbus::{INTERFACE, NAME, PATH};
use crate::paths::Dirs;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zbus::zvariant::Value;

pub const APP_ID: &str = "com.maprisma.Dictation.Omarchy";

fn pid_file(dirs: &Dirs) -> std::io::Result<PathBuf> {
    Ok(crate::tempwav::private_folder(dirs)?.join("omarchy.pid"))
}

/// The pid of a running `omarchy-run`, if any (stale files are removed).
pub fn running(dirs: &Dirs) -> Option<i32> {
    let file = pid_file(dirs).ok()?;
    let pid: i32 = std::fs::read_to_string(&file).ok()?.trim().parse().ok()?;
    if pid > 1 && is_runner(Path::new(&format!("/proc/{pid}"))) {
        Some(pid)
    } else {
        let _ = std::fs::remove_file(&file);
        None
    }
}

/// `/proc/<pid>` belongs to one of ours: same uid and `maprisma-voice omarchy-run` in its cmdline.
fn is_runner(proc_dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: getuid never fails.
    let uid = unsafe { libc::getuid() };
    if std::fs::metadata(proc_dir).ok().map(|m| m.uid()) != Some(uid) {
        return false;
    }
    let Ok(cmdline) = std::fs::read(proc_dir.join("cmdline")) else { return false };
    let args: Vec<&[u8]> = cmdline.split(|b| *b == 0).collect();
    args.first().is_some_and(|a| a.ends_with(b"maprisma-voice")) && args.get(1) == Some(&&b"omarchy-run"[..])
}

/// `start`: a detached runner; returns at once. Already running: nothing to do.
pub fn start(dirs: &Dirs) -> i32 {
    if running(dirs).is_some() {
        return 0;
    }
    let Ok(exe) = std::env::current_exe() else { return 1 };
    let mut command = std::process::Command::new(exe);
    command.arg("omarchy-run").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid is async-signal-safe; the runner must outlive Omarchy's short-lived adapter.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match command.spawn() {
        Ok(child) => {
            // Reaped by init once we exit; we return at once as Omarchy asks.
            std::mem::forget(child);
            0
        }
        Err(e) => {
            eprintln!("maprisma-voice omarchy start: {e}");
            1
        }
    }
}

/// `stop`: finish the running session; succeeds when there is none.
pub fn stop(dirs: &Dirs) -> i32 {
    if let Some(pid) = running(dirs) {
        // SAFETY: kill(2) on a process checked to be our own runner.
        unsafe { libc::kill(pid, libc::SIGUSR1) };
    }
    0
}

pub fn toggle(dirs: &Dirs) -> i32 {
    if running(dirs).is_some() { stop(dirs) } else { start(dirs) }
}

/// Types `text` into the focused window (wtype), else copies it (wl-copy). argv, no shell.
pub fn deliver(dirs: &Dirs, text: &str) -> Result<&'static str, String> {
    if text.trim().is_empty() {
        return Ok("nothing");
    }
    if let Some(wtype) = dirs.which("wtype") {
        let status = std::process::Command::new(wtype).arg("--").arg(text).stdin(std::process::Stdio::null()).status().map_err(|e| e.to_string())?;
        if status.success() {
            return Ok("typed");
        }
    }
    if let Some(copy) = dirs.which("wl-copy") {
        use std::io::Write;
        let mut child = std::process::Command::new(copy).stdin(std::process::Stdio::piped()).spawn().map_err(|e| e.to_string())?;
        child.stdin.take().map(|mut s| s.write_all(text.as_bytes())).transpose().map_err(|e| e.to_string())?;
        if child.wait().map_err(|e| e.to_string())?.success() {
            return Ok("copied");
        }
    }
    Err("neither wtype nor wl-copy worked".into())
}

/// The detached runner: one session, finished by SIGUSR1, cancelled by SIGTERM/SIGINT.
pub async fn run(dirs: Dirs) -> i32 {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(file) = pid_file(&dirs) else { return 1 };
    // O_EXCL: two starts at once leave one runner.
    let mut f = match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file) {
        Ok(f) => f,
        Err(_) if running(&dirs).is_some() => return 0,
        Err(_) => {
            let _ = std::fs::remove_file(&file);
            match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file) {
                Ok(f) => f,
                Err(_) => return 1,
            }
        }
    };
    let _ = write!(f, "{}", std::process::id());
    drop(f);
    let code = session(&dirs).await;
    let _ = std::fs::remove_file(&file);
    code
}

async fn session(dirs: &Dirs) -> i32 {
    let Ok(connection) = zbus::Connection::session().await else { return 4 };
    let rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(INTERFACE).unwrap().path(PATH).unwrap().build();
    let Ok(mut stream) = zbus::MessageStream::for_match_rule(rule, &connection, Some(1024)).await else { return 4 };
    let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
    opts.insert("end", "release".into());
    let reply = connection.call_method(Some(NAME), PATH, Some(INTERFACE), "StartSession", &(APP_ID, "dictate", opts)).await;
    let id: String = match reply.and_then(|r| r.body().deserialize::<String>()) {
        Ok(id) => id,
        Err(_) => return 1,
    };
    let call = |method: &'static str| {
        let (connection, id) = (connection.clone(), id.clone());
        async move {
            let _ = connection.call_method(Some(NAME), PATH, Some(INTERFACE), method, &(id.as_str(),)).await;
        }
    };
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut usr1), Ok(mut term), Ok(mut int)) = (signal(SignalKind::user_defined1()), signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else { return 1 };
    let mut text = String::new();
    // A forgotten runner never holds the microphone: the service caps a session at 60 s anyway.
    let deadline = tokio::time::sleep(Duration::from_secs(120));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            message = stream.next() => {
                let Some(Ok(message)) = message else { return 1 };
                match crate::client::decode(&message) {
                    crate::client::Signal::Final(sid, t, typed, _) if sid == id => { if !typed { text = t } }
                    crate::client::Signal::Closed(sid, _) if sid == id => break,
                    _ => {}
                }
            }
            _ = usr1.recv() => call("Finish").await,
            _ = term.recv() => call("Cancel").await,
            _ = int.recv() => call("Cancel").await,
            _ = &mut deadline => { call("Cancel").await; return 1 }
        }
    }
    match deliver(dirs, &text) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("maprisma-voice omarchy: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::testdir::TestDir;

    #[test]
    fn runner_check_needs_our_uid_and_cmdline() {
        let t = TestDir::new("omarchy");
        let fake = t.path().join("proc/42");
        std::fs::create_dir_all(&fake).unwrap();
        std::fs::write(fake.join("cmdline"), b"/home/u/.local/bin/maprisma-voice\0omarchy-run\0").unwrap();
        assert!(is_runner(&fake));
        std::fs::write(fake.join("cmdline"), b"/usr/bin/sleep\0100\0").unwrap();
        assert!(!is_runner(&fake));
        std::fs::write(fake.join("cmdline"), b"/x/maprisma-voice\0daemon\0").unwrap();
        assert!(!is_runner(&fake));
    }

    #[test]
    fn stop_succeeds_when_idle_and_stale_pid_files_go() {
        let t = TestDir::new("omarchy-idle");
        let dirs = Dirs::rooted(t.path(), "/usr/bin:/bin");
        assert_eq!(stop(&dirs), 0);
        let file = pid_file(&dirs).unwrap();
        std::fs::write(&file, "999999999").unwrap();
        assert_eq!(running(&dirs), None);
        assert!(!file.exists());
    }

    #[test]
    fn deliver_types_with_wtype_as_one_argument() {
        let t = TestDir::new("omarchy-type");
        let bin = t.script("wtype", "#!/bin/sh\nfor a in \"$@\"; do printf '[%s]' \"$a\"; done > \"$(dirname \"$0\")/typed\"\n");
        let dirs = Dirs::rooted(t.path(), &bin.display().to_string());
        assert_eq!(deliver(&dirs, "Hallo; rm -rf ~ $(id)"), Ok("typed"));
        assert_eq!(std::fs::read_to_string(bin.join("typed")).unwrap(), "[--][Hallo; rm -rf ~ $(id)]");
        assert_eq!(deliver(&dirs, "  "), Ok("nothing"));
    }

    #[test]
    fn deliver_falls_back_to_the_clipboard() {
        let t = TestDir::new("omarchy-copy");
        let bin = t.script("wl-copy", "#!/bin/sh\ncat > \"$(dirname \"$0\")/clip\"\n");
        let dirs = Dirs::rooted(t.path(), &bin.display().to_string());
        assert_eq!(deliver(&dirs, "Guten Morgen"), Ok("copied"));
        assert_eq!(std::fs::read_to_string(bin.join("clip")).unwrap(), "Guten Morgen");
    }
}
