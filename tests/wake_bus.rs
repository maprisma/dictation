//! The wake phrase end to end: the real `maprisma-voice daemon` on a private session bus, in a
//! temporary home, with a fake recorder that streams a Kokoro recording of "Hey Linux"
//! (tests/fixtures/wake, 16 kHz s16le) and a fake `maprisma-dictation` that logs its arguments.
//!
//!   dbus-run-session -- cargo test -p maprisma-voice --test wake_bus -- --ignored
//!
//! `fake_recogniser_on_a_bus` uses a fake whisper-server; `real_model_hears_kokoro` the
//! installed whisper.cpp and ggml-base (skipped when they are missing).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use zbus::zvariant::{OwnedValue, Value};

const NAME: &str = "com.maprisma.Voice1";
const PATH: &str = "/com/maprisma/Voice1";
const INTERFACE: &str = "com.maprisma.Voice1";

/// The tests own com.maprisma.Voice1 on the same bus: one at a time.
static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/wake");

/// Streams 16 kHz s16le in real time: 1.5 s quiet, the fixture (only the first time: a marker
/// file), then quiet until killed.
const FAKE_RECORDER: &str = r#"#!/usr/bin/env python3
import os, sys, time
here = os.path.dirname(os.path.abspath(__file__))
fixture = open(os.path.join(here, 'fixture')).read().strip()
marker = os.path.join(here, 'played')
open(os.path.join(here, 'recorder.log'), 'a').write('START\n')
out = sys.stdout.buffer
chunk = 1600  # 50 ms
def quiet(ms):
    for _ in range(ms // 50):
        out.write(b'\0' * chunk); out.flush(); time.sleep(0.05)
quiet(1500)
if fixture and not os.path.exists(marker):
    open(marker, 'w').close()
    data = open(fixture, 'rb').read()
    for i in range(0, len(data), chunk):
        out.write(data[i:i + chunk]); out.flush(); time.sleep(0.05)
while True:
    quiet(1000)
"#;

/// Answers every /inference with "Hey Linux, open Firefox." (and logs the language it got).
const FAKE_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, os, http.server
args = sys.argv[1:]
port = int(args[args.index('--port') + 1])
here = os.path.dirname(os.path.abspath(__file__))
open(os.path.join(here, 'server.log'), 'a').write('START %s\n' % ' '.join(args))
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(b'{}')
    def do_POST(self):
        self.rfile.read(int(self.headers['Content-Length']))
        open(os.path.join(here, 'server.log'), 'a').write('POST\n')
        self.send_response(200); self.send_header('Content-Type', 'application/json'); self.end_headers()
        self.wfile.write(json.dumps({'text': ' Hey Linux, open Firefox.'}).encode())
http.server.HTTPServer(('127.0.0.1', port), H).serve_forever()
"#;

struct Home {
    root: PathBuf,
    bin: PathBuf,
}

impl Home {
    fn new(name: &str) -> Home {
        let root = std::env::temp_dir().join(format!("maprisma-voice-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(root.join("home/.local/bin")).unwrap();
        std::fs::create_dir_all(root.join("home/.local/share/maprisma-voice/models")).unwrap();
        std::fs::create_dir_all(root.join("run")).unwrap();
        let home = Home { root, bin };
        home.script(&home.bin.join("pw-record"), FAKE_RECORDER);
        let log = home.root.join("overlay.log");
        home.script(&home.root.join("home/.local/bin/maprisma-dictation"), &format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done >> {}\necho END >> {}\n", log.display(), log.display()));
        home
    }

    fn script(&self, path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn fixture(&self, name: &str) {
        std::fs::write(self.bin.join("fixture"), format!("{FIXTURES}/{name}")).unwrap();
        let _ = std::fs::remove_file(self.bin.join("played"));
    }

    fn settings(&self, value: serde_json::Value) {
        let dir = self.root.join("home/.config/maprisma-voice");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), value.to_string()).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_maprisma-voice"));
        let home = self.root.join("home");
        c.args(args)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("LANG", "en_US.UTF-8")
            .env_remove("OPENAI_API_KEY")
            .env_remove("GROQ_API_KEY");
        c
    }

    fn overlay_log(&self) -> String {
        std::fs::read_to_string(self.root.join("overlay.log")).unwrap_or_default()
    }
}

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        // SAFETY: kill(2) on our own child.
        unsafe { libc::kill(self.0.id() as i32, libc::SIGTERM) };
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if let Ok(Some(_)) = self.0.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn start(home: &Home) -> (Daemon, zbus::Connection, zbus::MessageStream) {
    let connection = zbus::Connection::session().await.expect("a session bus: run under dbus-run-session");
    let dbus = zbus::fdo::DBusProxy::new(&connection).await.unwrap();
    assert!(!dbus.name_has_owner(NAME.try_into().unwrap()).await.unwrap(), "a voice service runs on this bus already");
    let rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(INTERFACE).unwrap().path(PATH).unwrap().build();
    let stream = zbus::MessageStream::for_match_rule(rule, &connection, Some(1024)).await.unwrap();
    let child = home.command(&["daemon"]).stdin(Stdio::null()).stdout(Stdio::null()).spawn().unwrap();
    let daemon = Daemon(child);
    let until = Instant::now() + Duration::from_secs(10);
    while !dbus.name_has_owner(NAME.try_into().unwrap()).await.unwrap() {
        assert!(Instant::now() < until, "the daemon did not start");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (daemon, connection, stream)
}

async fn status(connection: &zbus::Connection) -> serde_json::Map<String, serde_json::Value> {
    let reply = connection.call_method(Some(NAME), PATH, Some(INTERFACE), "Status", &()).await.unwrap();
    let map: HashMap<String, OwnedValue> = reply.body().deserialize().unwrap();
    map.iter()
        .map(|(k, v)| {
            let j = match &**v {
                Value::Bool(b) => serde_json::Value::from(*b),
                Value::Str(s) => serde_json::Value::from(s.as_str()),
                _ => serde_json::Value::Null,
            };
            (k.clone(), j)
        })
        .collect()
}

async fn wait_status(connection: &zbus::Connection, what: &str, seconds: u64, ok: impl Fn(&serde_json::Map<String, serde_json::Value>) -> bool) {
    let until = Instant::now() + Duration::from_secs(seconds);
    loop {
        let s = status(connection).await;
        if ok(&s) {
            return;
        }
        assert!(Instant::now() < until, "timed out waiting for {what}: {s:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The next `Wake(phrase, rest)`; also notes whether `WakeListening(true)` came before it.
async fn next_wake(stream: &mut zbus::MessageStream, seconds: u64) -> (String, String, bool) {
    use futures_util::StreamExt;
    let mut listening = false;
    let until = tokio::time::Instant::now() + Duration::from_secs(seconds);
    loop {
        let message = tokio::time::timeout_at(until, stream.next()).await.expect("no Wake signal").unwrap().unwrap();
        let header = message.header();
        match header.member().map(|m| m.as_str()) {
            Some("WakeListening") => listening |= message.body().deserialize::<bool>().unwrap(),
            Some("Wake") => {
                let (phrase, rest): (String, String) = message.body().deserialize().unwrap();
                return (phrase, rest, listening);
            }
            _ => {}
        }
    }
}

async fn wait_overlay(home: &Home, ends: usize) -> String {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let log = home.overlay_log();
        if log.matches("END").count() >= ends {
            return log;
        }
        assert!(Instant::now() < until, "the overlay was not started: {log:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
#[ignore = "needs a session bus: dbus-run-session -- cargo test -p maprisma-voice --test wake_bus -- --ignored"]
async fn fake_recogniser_on_a_bus() {
    let _turn = TURN.lock().await;
    let home = Home::new("wake-bus");
    home.script(&home.bin.join("whisper-server"), FAKE_SERVER);
    std::fs::write(home.root.join("home/.local/share/maprisma-voice/models/ggml-tiny.bin"), "x").unwrap();
    home.fixture("hey-linux.raw");
    home.settings(serde_json::json!({"wake_enabled": true, "whisper_server": home.bin.join("whisper-server").to_str().unwrap()}));
    let (_daemon, connection, mut stream) = start(&home).await;
    let (phrase, rest, listening) = next_wake(&mut stream, 20).await;
    assert_eq!((phrase.as_str(), rest.as_str(), listening), ("Hey Linux", "open Firefox.", true));
    assert_eq!(wait_overlay(&home, 1).await, "--wake\n--ask-rest\nopen Firefox.\nEND\n");
    // The wake recogniser runs on the CPU with 4 threads, on its own server.
    let server_log = std::fs::read_to_string(home.bin.join("server.log")).unwrap();
    assert!(server_log.lines().next().unwrap().contains("-t 4 -nt -ng"), "{server_log}");
    let s = status(&connection).await;
    assert_eq!((s["wake_enabled"].as_bool(), s["wake_phrase"].as_str()), (Some(true), Some("Hey Linux")));
    // After the cooldown it listens again.
    wait_status(&connection, "listening again", 10, |s| s["wake_active"] == true && s["wake_why_not"] == "").await;
    // A session from another app: the listener steps back, and comes back after it.
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    let reply = connection.call_method(Some(NAME), PATH, Some(INTERFACE), "StartSession", &("com.maprisma.Test", "dictate", options)).await.unwrap();
    let id: String = reply.body().deserialize().unwrap();
    let s = status(&connection).await;
    assert_eq!((s["state"].as_str(), s["wake_active"].as_bool(), s["wake_why_not"].as_str()), (Some("listening"), Some(false), Some("busy")));
    connection.call_method(Some(NAME), PATH, Some(INTERFACE), "Cancel", &(id.as_str(),)).await.unwrap();
    wait_status(&connection, "back after the session", 10, |s| s["wake_active"] == true).await;
    // `wake test` (the Manager's "Try the phrase"): the listener pauses, nothing is started.
    // (Past the listener's recorder's 1.5 s of quiet, so only the test's recorder plays it.)
    tokio::time::sleep(Duration::from_secs(2)).await;
    home.fixture("hey-linux.raw");
    let out = home.command(&["wake", "test", "--json", "--seconds", "5"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!((result["matched"].as_bool(), result["heard"].as_str(), result["rest"].as_str()), (Some(true), Some("Hey Linux, open Firefox."), Some("open Firefox.")));
    assert_eq!(result["model"], "ggml-tiny.bin");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(home.overlay_log().matches("END").count(), 1, "a test never opens the overlay");
    // Turned off: the microphone closes.
    home.settings(serde_json::json!({"wake_enabled": false}));
    wait_status(&connection, "off", 5, |s| s["wake_active"] == false && s["wake_why_not"] == "off").await;
    let _ = std::fs::remove_dir_all(&home.root);
}

#[tokio::test]
#[ignore = "needs a session bus, whisper.cpp and ggml-base: dbus-run-session -- cargo test -p maprisma-voice --test wake_bus -- --ignored"]
async fn real_model_hears_kokoro() {
    let _turn = TURN.lock().await;
    let real = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".local/share/maprisma-voice");
    let (server, model) = (real.join("whisper.cpp/bin/whisper-server"), real.join("models/ggml-base.bin"));
    if !server.is_file() || !model.exists() {
        eprintln!("whisper.cpp or ggml-base missing: skipped");
        return;
    }
    let home = Home::new("wake-real");
    std::os::unix::fs::symlink(&model, home.root.join("home/.local/share/maprisma-voice/models/ggml-base.bin")).unwrap();
    home.fixture("hey-linux.raw");
    home.settings(serde_json::json!({"wake_enabled": true, "whisper_server": server.to_str().unwrap()}));
    let (_daemon, connection, mut stream) = start(&home).await;
    let started = Instant::now();
    let (phrase, rest, _) = next_wake(&mut stream, 60).await;
    eprintln!("Wake({phrase:?}, {rest:?}) after {:.1} s (with loading the model)", started.elapsed().as_secs_f64());
    assert_eq!((phrase.as_str(), rest.as_str()), ("Hey Linux", ""));
    assert_eq!(wait_overlay(&home, 1).await, "--wake\nEND\n");
    // Not the phrase: no wake.
    home.fixture("hallo-welt.raw");
    let out = home.command(&["wake", "test", "--json", "--seconds", "8"]).output().unwrap();
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    eprintln!("wake test (Hallo Welt): {result}");
    assert_eq!(result["matched"], false, "{}", String::from_utf8_lossy(&out.stderr));
    // The phrase with a request (English recognition for the phrase's language).
    home.settings(serde_json::json!({"wake_enabled": true, "wake_language": "en", "whisper_server": server.to_str().unwrap()}));
    home.fixture("hey-linux-oeffne-firefox.raw");
    let started = Instant::now();
    let out = home.command(&["wake", "test", "--json", "--seconds", "8"]).output().unwrap();
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    eprintln!("wake test (Hey Linux, öffne Firefox) in {:.1} s: {result}", started.elapsed().as_secs_f64());
    assert_eq!(result["matched"], true, "{}", String::from_utf8_lossy(&out.stderr));
    assert!(result["rest"].as_str().unwrap_or("").to_lowercase().contains("firefox"), "{result}");
    drop(connection);
    let _ = std::fs::remove_dir_all(&home.root);
}
