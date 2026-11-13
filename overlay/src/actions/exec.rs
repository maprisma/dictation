//! Carrying out one action (blocking; the UI runs it off its thread). Programs run without a
//! shell; apps and URLs start in a session of their own so they outlive the overlay.

use super::apps::{self, App, Found};
use super::desktop::{self, HyprWindow, Plan, Step};
use super::schema::{Action, Media, ThemeMode};
use super::system::{self, ThemePick};
use super::weather::{self, LookupError, Weather};
use crate::look::Desktop;
use maprisma_voice::i18n::{tr, trf};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// run_command: stopped after this long; output kept up to this many bytes.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(60);
pub const OUTPUT_MAX: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct Context {
    pub desktop: Desktop,
    /// The overlay's language code (weather words, place names).
    pub language: String,
    pub weather_location: String,
}

/// What a confirmation card or a run aims at, looked up when the answer arrives.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Target {
    pub window: Option<HyprWindow>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Undo {
    Volume(f64, bool),
    Dnd(bool),
    Theme(String),
    Workspace(u32),
    /// Hyprland: the window back to its workspace.
    MoveWindow(String, u32),
    PlayPause,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Done { text: String, undo: Option<Undo> },
    Weather(Box<Weather>),
    /// run_command finished (or not): its output.
    Output { ok: bool, text: String, output: String },
    Choices(Vec<App>),
    Failed(String),
    Unsupported(String),
}

pub struct Ran {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Runs argv (no shell) in its own process group; the group is killed after `timeout`. When
/// the program itself ends, whatever it left running in its group is stopped too (SIGTERM, then
/// SIGKILL), and the output readers are closed: nothing outlives the action.
pub fn run_capture(argv: &[String], timeout: Duration, cwd: Option<&std::path::Path>) -> std::io::Result<Ran> {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).env("NO_COLOR", "1").process_group(0);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn()?;
    let pid = child.id() as i32;
    let out = child.stdout.take().expect("piped");
    let err = child.stderr.take().expect("piped");
    let out_thread = read_bounded(out);
    let err_thread = read_bounded(err);
    let started = Instant::now();
    let mut timed_out = false;
    // The leader is only reaped after the group is cleaned up: until then its pid (= the group
    // id) can't be reused, so the group signals never hit anyone else.
    loop {
        if exited(pid) {
            break;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !timed_out {
        // Output still coming from what it started: a short moment to finish, then it is stopped.
        let _ = out_thread.wait(Duration::from_millis(300));
    }
    crate::procgroup::terminate(pid, Duration::from_millis(500));
    let status = child.wait()?;
    let stdout = out_thread.take(Duration::from_millis(500));
    let stderr = err_thread.take(Duration::from_millis(200));
    Ok(Ran { ok: status.success() && !timed_out, stdout, stderr, timed_out })
}

/// Whether child `pid` has exited, without reaping it (`WNOWAIT`).
fn exited(pid: i32) -> bool {
    // SAFETY: waitid on our own child with a zeroed siginfo it fills.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let r = libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT);
        r == 0 && info.si_pid() != 0 || r != 0
    }
}

/// Reads up to OUTPUT_MAX bytes on a thread; [Bounded::take] gives what came (waiting at most
/// `wait` for the end) and makes the thread let go of the pipe.
struct Bounded {
    kept: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    done: std::sync::mpsc::Receiver<()>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ended: std::cell::Cell<bool>,
}

impl Bounded {
    /// Whether the pipe ended within `wait`.
    fn wait(&self, wait: Duration) -> bool {
        if !self.ended.get() && self.done.recv_timeout(wait).is_ok() {
            self.ended.set(true);
        }
        self.ended.get()
    }

    fn take(self, wait: Duration) -> String {
        self.wait(wait);
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let kept = self.kept.lock().map(|k| k.clone()).unwrap_or_default();
        String::from_utf8_lossy(&kept).into_owned()
    }
}

fn read_bounded<R: Read + std::os::fd::AsRawFd + Send + 'static>(mut r: R) -> Bounded {
    let (sender, done) = std::sync::mpsc::channel();
    let kept = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (shared, stopped) = (kept.clone(), stop.clone());
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        // Polled, so `take` can end it even when something escaped the group keeps the pipe open.
        while !stopped.load(std::sync::atomic::Ordering::SeqCst) {
            let mut fd = libc::pollfd { fd: r.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd for an fd this thread owns.
            if unsafe { libc::poll(&mut fd, 1, 100) } <= 0 {
                continue;
            }
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut k) = shared.lock() {
                        let room = OUTPUT_MAX.saturating_sub(k.len());
                        k.extend_from_slice(&buf[..n.min(room)]);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = sender.send(());
    });
    Bounded { kept, done, stop, ended: std::cell::Cell::new(false) }
}

/// Starts argv in a session of its own (it outlives the overlay); waits up to `wait` for it to
/// fail early (gtk-launch exits once the app runs). Ok(()) when it is running or exited cleanly.
pub fn spawn_detached(argv: &[String], wait: Duration) -> Result<(), String> {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).current_dir(crate::paths::home());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // What is in the pipe now: the started app keeps it open, so never read to its end.
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    use std::os::fd::AsRawFd;
                    unsafe {
                        let fd = e.as_raw_fd();
                        libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
                    }
                    let mut buf = [0u8; 4096];
                    if let Ok(n) = e.read(&mut buf) {
                        err = String::from_utf8_lossy(&buf[..n]).into_owned();
                    }
                }
                return if status.success() { Ok(()) } else { Err(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().chars().take(200).collect()) };
            }
            Ok(None) if started.elapsed() < wait => std::thread::sleep(Duration::from_millis(25)),
            _ => break,
        }
    }
    // Still running: let it be, and reap it when it ends.
    std::thread::spawn(move || {
        drop(child.stderr.take());
        let _ = child.wait();
    });
    Ok(())
}

fn hyprctl_json(what: &str) -> Option<Vec<u8>> {
    run_capture(&["hyprctl".into(), "-j".into(), what.into()], Duration::from_secs(3), None).ok().filter(|r| r.ok).map(|r| r.stdout.into_bytes())
}

/// The window an action aims at (Hyprland: the active one; elsewhere the desktop picks).
pub fn prepare(action: &Action, desktop: Desktop) -> Target {
    match (action, desktop) {
        (Action::CloseWindow | Action::MoveWindow { .. }, Desktop::Hyprland) => Target { window: hyprctl_json("activewindow").and_then(|j| desktop::hypr_active_window(&j)) },
        _ => Target::default(),
    }
}

/// The busctl call for a KWin step (the KDE session bus).
pub fn busctl_argv(path: &str, interface: &str, method: &str, signature: &str, args: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = ["busctl", "--user", "--timeout=5", "call", "org.kde.KWin", path, interface, method].iter().map(|s| s.to_string()).collect();
    if !signature.is_empty() {
        v.push(signature.into());
        v.extend(args.iter().map(|s| s.to_string()));
    }
    v
}

/// `busctl` prints `i 5`.
pub fn busctl_int(out: &str) -> Option<i32> {
    out.trim().strip_prefix("i ")?.trim().parse().ok()
}

/// `busctl` prints `b true`.
pub fn busctl_bool(out: &str) -> Option<bool> {
    match out.trim().strip_prefix("b ")?.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// What a KWin script reported (None: nothing in time) → done or why not.
pub fn kwin_outcome(report: Option<&str>) -> Result<(), String> {
    match report {
        Some("ok") => Ok(()),
        Some("no-window") => Err(tr("There is no window to do that with.")),
        Some("no-desktop") => Err(tr("There is no such workspace.")),
        Some(other) => Err(other.chars().take(200).collect()),
        None => Err(tr("KWin did not confirm it.")),
    }
}

fn run_step(step: &Step) -> Result<(), String> {
    match step {
        Step::Hyprctl(args) => {
            let mut argv = vec!["hyprctl".to_string()];
            argv.extend(args.iter().cloned());
            let r = run_capture(&argv, Duration::from_secs(5), None).map_err(|e| e.to_string())?;
            if r.ok && r.stdout.trim() == "ok" { Ok(()) } else { Err(first_line(&r.stdout, &r.stderr)) }
        }
        Step::KwinDesktop(n) => {
            let r = run_capture(&busctl_argv("/KWin", "org.kde.KWin", "setCurrentDesktop", "i", &[&n.to_string()]), Duration::from_secs(6), None).map_err(|e| e.to_string())?;
            // KWin answers false for a desktop it doesn't have.
            match (r.ok, busctl_bool(&r.stdout)) {
                (true, Some(true)) => Ok(()),
                (true, Some(false)) => Err(tr("There is no such workspace.")),
                _ => Err(first_line(&r.stdout, &r.stderr)),
            }
        }
        Step::KwinScript(js) => {
            let dir = crate::paths::runtime_dir().map_err(|e| e.to_string())?;
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let plugin = format!("maprisma_dictation_{}_{}", std::process::id(), SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
            let file = dir.join(format!("{plugin}.js"));
            system::write_atomic(&file, desktop::kwin_script(js, &plugin).as_bytes()).map_err(|e| e.to_string())?;
            let path = file.to_string_lossy().into_owned();
            let result = (|| {
                let r = run_capture(&busctl_argv("/Scripting", "org.kde.kwin.Scripting", "loadScript", "ss", &[&path, &plugin]), Duration::from_secs(6), None).map_err(|e| e.to_string())?;
                let id = busctl_int(&r.stdout).filter(|i| *i >= 0).ok_or_else(|| first_line(&r.stdout, &r.stderr))?;
                let r = run_capture(&busctl_argv(&format!("/Scripting/Script{id}"), "org.kde.kwin.Script", "run", "", &[]), Duration::from_secs(6), None).map_err(|e| e.to_string())?;
                if !r.ok {
                    return Err(first_line(&r.stdout, &r.stderr));
                }
                // The script reports through the overlay's KwinResult (desktop::kwin_script).
                kwin_outcome(crate::instance::wait_kwin_result(&plugin, Duration::from_secs(2)).as_deref())
            })();
            let _ = run_capture(&busctl_argv("/Scripting", "org.kde.kwin.Scripting", "unloadScript", "s", &[&plugin]), Duration::from_secs(6), None);
            let _ = std::fs::remove_file(&file);
            result
        }
    }
}

fn first_line(stdout: &str, stderr: &str) -> String {
    stderr.lines().chain(stdout.lines()).map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(200).collect()
}

fn run_plan(plan: Plan) -> Result<(), Option<String>> {
    let Plan::Try(steps) = plan else { return Err(None) };
    let mut last = String::new();
    for step in &steps {
        match run_step(step) {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
    }
    Err(Some(last))
}

fn unsupported_here() -> Outcome {
    Outcome::Unsupported(tr("This desktop has no way to do that for other apps."))
}

fn failed(e: impl std::fmt::Display) -> Outcome {
    let e = e.to_string();
    Outcome::Failed(if e.is_empty() { tr("It did not work.") } else { e })
}

fn run_all(cmds: &[Vec<String>]) -> Result<(), String> {
    for c in cmds {
        let r = run_capture(c, Duration::from_secs(5), None).map_err(|e| e.to_string())?;
        if !r.ok {
            return Err(first_line(&r.stdout, &r.stderr));
        }
    }
    Ok(())
}

fn current_volume() -> Option<(f64, bool)> {
    let r = run_capture(&["wpctl".into(), "get-volume".into(), system::SINK.into()], Duration::from_secs(3), None).ok()?;
    system::parse_volume(&r.stdout)
}

pub fn volume_text(state: (f64, bool)) -> String {
    let p = (state.0 * 100.0).round().to_string();
    if state.1 { trf("Volume {0} %, muted", &[&p]) } else { trf("Volume {0} %", &[&p]) }
}

/// Do not disturb is Maprisma Notifier's: its recorder applies the `dnd` key of its settings to
/// the desktop (COSMIC's key, KDE's inhibition, Maprisma Shell). Without it nothing would change.
fn set_dnd(on: bool) -> Result<bool, String> {
    let path = system::notifier_settings();
    // The Notifier's lock protocol for its settings file (an OFD lock on settings.json.lock).
    let _lock = system::JsonLock::take(&path).map_err(|e| e.to_string())?;
    let old = std::fs::read(&path).ok();
    let before = system::dnd_of(old.as_deref());
    system::write_atomic(&path, &system::with_key(old.as_deref(), "dnd", on.into())).map_err(|e| e.to_string())?;
    Ok(before)
}

fn apply_theme(slug: &str) -> Result<(), String> {
    let r = run_capture(&system::theme_apply_argv(slug), Duration::from_secs(30), None).map_err(|e| e.to_string())?;
    if let Some(e) = system::theme_apply_error(&r.stdout) {
        return Err(e);
    }
    if r.ok { Ok(()) } else { Err(first_line(&r.stdout, &r.stderr)) }
}

/// Starts the app the user picked (or the one found).
pub fn launch(app: &App) -> Outcome {
    match spawn_detached(&apps::launch_argv(app), Duration::from_secs(3)) {
        Ok(()) => Outcome::Done { text: trf("Opened {0}", &[&app.name]), undo: None },
        Err(e) => failed(trf("{0} could not be started: {1}", &[&app.name, &e])),
    }
}

/// Carries out an action (already allowed: level 0, or level 1 after Enter).
pub fn execute(action: &Action, target: &Target, ctx: &Context) -> Outcome {
    match action {
        Action::OpenApp { name } => match apps::resolve(&apps::installed(&ctx.language), name) {
            Found::One(app) => launch(&app),
            Found::Choices(list) => Outcome::Choices(list),
            Found::None => failed(trf("No installed app is called “{0}”.", &[name])),
        },
        Action::OpenUrl { url } => match spawn_detached(&["xdg-open".into(), url.clone()], Duration::from_secs(2)) {
            Ok(()) => Outcome::Done { text: trf("Opened {0}", &[&host_of(url)]), undo: None },
            Err(e) => failed(e),
        },
        Action::Dnd { .. } if !system::notifier_installed(&std::env::var("PATH").unwrap_or_default()) => Outcome::Unsupported(tr("Maprisma Notifier is not installed.")),
        Action::Dnd { on } => match set_dnd(*on) {
            Ok(before) => Outcome::Done { text: if *on { tr("Do not disturb is on") } else { tr("Do not disturb is off") }, undo: (before != *on).then_some(Undo::Dnd(before)) },
            Err(e) => failed(e),
        },
        Action::ThemeMode { mode } => {
            let light = *mode == ThemeMode::Light;
            if crate::helper::which_in("maprisma-theme-core", &std::env::var("PATH").unwrap_or_default()).is_none() {
                return Outcome::Unsupported(tr("Maprisma Theme is not installed."));
            }
            let files = system::theme_files();
            let themes = system::scan_themes(&files);
            let current = system::current_theme(std::fs::read(&files.current).ok().as_deref());
            let recent = std::fs::read(&files.recent).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default();
            match system::pick_theme(&themes, current.as_deref(), &recent, light) {
                ThemePick::Already(slug) => Outcome::Done { text: trf(if light { "Already a light theme ({0})" } else { "Already a dark theme ({0})" }, &[&slug]), undo: None },
                ThemePick::NoneOfThatMode => failed(if light { tr("Maprisma Theme has no light theme.") } else { tr("Maprisma Theme has no dark theme.") }),
                ThemePick::Switch(slug) => match apply_theme(&slug) {
                    Ok(()) => Outcome::Done { text: trf(if light { "Light theme: {0}" } else { "Dark theme: {0}" }, &[&slug]), undo: current.map(Undo::Theme) },
                    Err(e) => failed(e),
                },
            }
        }
        Action::Volume { percent, mute, change } => {
            let Some(before) = current_volume() else { return failed(tr("The volume could not be read.")) };
            let (cmds, after) = system::volume_commands(before, *percent, *mute, *change);
            match run_all(&cmds) {
                Ok(()) => Outcome::Done { text: volume_text(after), undo: Some(Undo::Volume(before.0, before.1)) },
                Err(e) => failed(e),
            }
        }
        Action::Media { command } => {
            let arg = match command {
                Media::PlayPause => "play-pause",
                Media::Next => "next",
                Media::Previous => "previous",
            };
            match run_capture(&["playerctl".into(), arg.into()], Duration::from_secs(5), None) {
                Ok(r) if r.ok => Outcome::Done {
                    text: match command {
                        Media::PlayPause => tr("Play/pause"),
                        Media::Next => tr("Next track"),
                        Media::Previous => tr("Previous track"),
                    },
                    undo: (*command == Media::PlayPause).then_some(Undo::PlayPause),
                },
                Ok(r) if r.stderr.contains("No players found") => failed(tr("No media player is running.")),
                Ok(r) => failed(first_line(&r.stdout, &r.stderr)),
                Err(e) => failed(e),
            }
        }
        Action::Workspace { number } => {
            let before = match ctx.desktop {
                Desktop::Hyprland => hyprctl_json("activeworkspace").and_then(|j| desktop::hypr_workspace_id(&j)),
                Desktop::Kde => run_capture(&busctl_argv("/KWin", "org.kde.KWin", "currentDesktop", "", &[]), Duration::from_secs(3), None).ok().and_then(|r| busctl_int(&r.stdout)).filter(|n| *n >= 1).map(|n| n as u32),
                _ => None,
            };
            match run_plan(desktop::workspace(ctx.desktop, *number)) {
                Ok(()) => Outcome::Done { text: trf("Workspace {0}", &[&number.to_string()]), undo: before.filter(|b| b != number).map(Undo::Workspace) },
                Err(None) => unsupported_here(),
                Err(Some(e)) => failed(e),
            }
        }
        Action::MoveWindow { workspace } => {
            let window = target.window.as_ref();
            match run_plan(desktop::move_window(ctx.desktop, *workspace, window.map(|w| w.address.as_str()))) {
                Ok(()) => Outcome::Done {
                    text: trf("Moved {0} to workspace {1}", &[&window.map(|w| w.class.clone()).filter(|c| !c.is_empty()).unwrap_or_else(|| tr("the window")), &workspace.to_string()]),
                    undo: window.filter(|w| w.workspace >= 1 && w.workspace != *workspace).map(|w| Undo::MoveWindow(w.address.clone(), w.workspace)),
                },
                Err(None) if ctx.desktop == Desktop::Hyprland => failed(tr("There is no window to move.")),
                Err(None) => unsupported_here(),
                Err(Some(e)) => failed(e),
            }
        }
        Action::CloseWindow => {
            let window = target.window.as_ref();
            match run_plan(desktop::close_window(ctx.desktop, window.map(|w| w.address.as_str()))) {
                Ok(()) => Outcome::Done { text: trf("Closed {0}", &[&window.map(|w| w.class.clone()).filter(|c| !c.is_empty()).unwrap_or_else(|| tr("the window"))]), undo: None },
                Err(None) if ctx.desktop == Desktop::Hyprland => failed(tr("There is no window to close.")),
                Err(None) => unsupported_here(),
                Err(Some(e)) => failed(e),
            }
        }
        Action::Weather { location } => match weather::lookup(location, &ctx.language, &ctx.weather_location) {
            Ok(w) => Outcome::Weather(Box::new(w)),
            Err(LookupError::NoLocation) => failed(tr("Which place? Say a city, or set weather_location.")),
            Err(LookupError::NoPlace(p)) => failed(trf("No place called “{0}” was found.", &[&p])),
            Err(LookupError::Network(e)) => failed(trf("The weather could not be fetched: {0}", &[&e])),
        },
        Action::RunCommand { argv, .. } => match run_capture(argv, RUN_TIMEOUT, Some(&crate::paths::home())) {
            Ok(r) => {
                let mut output = r.stdout.trim_end().to_string();
                if !r.stderr.trim().is_empty() {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(r.stderr.trim_end());
                }
                let text = if r.timed_out {
                    tr("Stopped after 60 s")
                } else if r.ok {
                    tr("Command finished")
                } else {
                    tr("Command failed")
                };
                Outcome::Output { ok: r.ok, text, output }
            }
            Err(e) => failed(trf("{0} could not be started: {1}", &[&argv[0], &e.to_string()])),
        },
    }
}

/// Takes an action back.
pub fn undo(u: &Undo, ctx: &Context) -> Result<(), String> {
    match u {
        Undo::Volume(v, m) => run_all(&system::volume_restore((*v, *m))),
        Undo::Dnd(before) => set_dnd(*before).map(|_| ()),
        Undo::Theme(slug) => apply_theme(slug),
        Undo::Workspace(n) => run_plan(desktop::workspace(ctx.desktop, *n)).map_err(|e| e.unwrap_or_default()),
        Undo::MoveWindow(address, n) => run_plan(desktop::move_window(ctx.desktop, *n, Some(address))).map_err(|e| e.unwrap_or_default()),
        Undo::PlayPause => run_all(&[vec!["playerctl".into(), "play-pause".into()]]),
    }
}

pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split(['/', '?', '#']).next().unwrap_or(rest).rsplit('@').next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_without_a_shell() {
        let r = run_capture(&["printf".into(), "%s|".into(), "a; $(x)".into(), "b".into()], Duration::from_secs(5), Some(std::path::Path::new("/"))).unwrap();
        assert!(r.ok);
        assert_eq!(r.stdout, "a; $(x)|b|");
        let r = run_capture(&["sleep".into(), "5".into()], Duration::from_millis(200), None).unwrap();
        assert!(r.timed_out && !r.ok);
        let r = run_capture(&["sh".into(), "-c".into(), "head -c 100000 /dev/zero".into()], Duration::from_secs(5), None).unwrap();
        assert_eq!(r.stdout.len(), OUTPUT_MAX);
        assert!(run_capture(&["/nonexistent/program".into()], Duration::from_secs(1), None).is_err());
        let started = Instant::now();
        let r = run_capture(&["sh".into(), "-c".into(), "setsid sleep 4 & echo hi".into()], Duration::from_secs(5), None).unwrap();
        assert!(r.ok && started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
        assert_eq!(r.stdout, "hi\n");
        assert!(spawn_detached(&["false".into()], Duration::from_secs(2)).is_err());
        assert!(spawn_detached(&["true".into()], Duration::from_secs(2)).is_ok());
        // A launcher that exits while what it started keeps stderr open must not hang us.
        let started = Instant::now();
        assert!(spawn_detached(&["sh".into(), "-c".into(), "sleep 3 & echo started >&2".into()], Duration::from_secs(2)).is_ok());
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    }

    fn alive(pid: i32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.rsplit(')').next().unwrap_or("").trim_start().starts_with('Z'))
    }

    /// What the program leaves running in its group ends with it, even when it ignores SIGTERM
    /// and keeps the output pipe open.
    #[test]
    fn background_descendants_end_with_the_action() {
        let started = Instant::now();
        let r = run_capture(&["sh".into(), "-c".into(), "sleep 30 & echo $!".into()], Duration::from_secs(10), None).unwrap();
        assert!(r.ok && started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
        let pid: i32 = r.stdout.trim().parse().unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while alive(pid) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(pid), "the background sleep still runs");
        // ignores SIGTERM and keeps stdout open: killed after the grace period
        let started = Instant::now();
        let r = run_capture(&["sh".into(), "-c".into(), "trap '' TERM; sleep 30 & echo $!".into()], Duration::from_secs(10), None).unwrap();
        assert!(r.ok && started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
        let pid: i32 = r.stdout.trim().parse().unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while alive(pid) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(pid), "the TERM-ignoring sleep still runs");
    }

    #[test]
    fn kwin_calls_and_small_parsers() {
        assert_eq!(
            busctl_argv("/Scripting", "org.kde.kwin.Scripting", "loadScript", "ss", &["/run/x.js", "p"]),
            ["busctl", "--user", "--timeout=5", "call", "org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting", "loadScript", "ss", "/run/x.js", "p"]
        );
        assert_eq!(busctl_argv("/KWin", "org.kde.KWin", "currentDesktop", "", &[]).len(), 8);
        assert_eq!(busctl_int("i 5\n"), Some(5));
        assert_eq!(busctl_int("s \"x\""), None);
        assert_eq!(busctl_bool("b true\n"), Some(true));
        assert_eq!(busctl_bool("b false"), Some(false));
        assert_eq!(busctl_bool("i 1"), None);
        assert_eq!(kwin_outcome(Some("ok")), Ok(()));
        assert!(kwin_outcome(Some("no-window")).is_err() && kwin_outcome(Some("no-desktop")).is_err());
        assert_eq!(kwin_outcome(Some("error: TypeError")), Err("error: TypeError".into()));
        assert!(kwin_outcome(None).is_err());
        assert_eq!(host_of("https://user@example.org:8080/a?b"), "example.org:8080");
    }
}
