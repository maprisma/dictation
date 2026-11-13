//! `maprisma-voice`: the daemon and its command line. See README.md and docs/protocol.md.

use maprisma_voice::client::{self, DictateOptions};
use maprisma_voice::paths::Dirs;
use maprisma_voice::{audio, i18n, models, service, settings};
use std::process::ExitCode;

const USAGE: &str = "maprisma-voice — the Maprisma voice service

  maprisma-voice daemon                       run the D-Bus service com.maprisma.Voice1
  maprisma-voice dictate [--silence | --seconds N] [--language CODE] [--assistant]
                                              speak; partial text on stderr, the final text on stdout
  maprisma-voice transcribe FILE.wav [--engine E] [--model M] [--language CODE]
                                              a 16-bit PCM WAV file to text, without the daemon
  maprisma-voice status [--json]              engine, model, language and why it is not ready
  maprisma-voice models [--json]              catalog and installed models
  maprisma-voice model download|remove NAME   a catalog model (base, small, large-v3-turbo)
  maprisma-voice get [KEY]                    settings (JSON)
  maprisma-voice set KEY VALUE                one setting; VALUE is JSON or a plain string
  maprisma-voice unset KEY                    back to the default
  maprisma-voice --version
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dirs = Dirs::from_env();
    i18n::set_language(&settings::load(&dirs).ui_language);
    let code = match args.first().map(String::as_str) {
        Some("daemon") => runtime(async move {
            match maprisma_voice::dbus::run_daemon(dirs).await {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("maprisma-voice: {e}");
                    1
                }
            }
        }),
        Some("dictate") => dictate(&args[1..]),
        Some("transcribe") => transcribe(&dirs, &args[1..]),
        Some("status") => status(&args[1..]),
        Some("models") => {
            let listing = models::listing(&dirs, &settings::load(&dirs).model);
            if args.iter().any(|a| a == "--json") {
                println!("{listing}");
            } else {
                print_models(&listing);
            }
            0
        }
        Some("model") => model(&dirs, &args[1..]),
        Some("get") => {
            let value = settings::read_value(&dirs);
            match args.get(1) {
                Some(key) => println!("{}", value.get(key).cloned().unwrap_or(serde_json::Value::Null)),
                None => println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default()),
            }
            0
        }
        Some("set") if args.len() == 3 => {
            let value = serde_json::from_str(&args[2]).unwrap_or_else(|_| serde_json::Value::String(args[2].clone()));
            set(&dirs, &args[1], Some(value))
        }
        Some("unset") if args.len() == 2 => set(&dirs, &args[1], None),
        Some("--version") | Some("version") => {
            println!("maprisma-voice {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("--help") | Some("-h") | Some("help") => {
            print!("{USAGE}");
            0
        }
        _ => {
            eprint!("{USAGE}");
            2
        }
    };
    ExitCode::from(code as u8)
}

fn runtime(future: impl std::future::Future<Output = i32>) -> i32 {
    match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt.block_on(future),
        Err(e) => {
            eprintln!("maprisma-voice: {e}");
            1
        }
    }
}

/// The value after `flag`, if any.
fn value_of<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn dictate(args: &[String]) -> i32 {
    let seconds = match value_of(args, "--seconds").map(str::parse::<f64>) {
        Some(Ok(s)) if s > 0.0 && s <= 60.0 => Some(s),
        Some(_) => {
            eprintln!("--seconds: 0 < N <= 60");
            return 2;
        }
        None => None,
    };
    let options = DictateOptions {
        silence: args.iter().any(|a| a == "--silence") || seconds.is_none(),
        seconds,
        language: value_of(args, "--language").map(str::to_string),
        mode: if args.iter().any(|a| a == "--assistant") { "assistant".into() } else { "dictate".into() },
        app_id: "com.maprisma.Voice.cli".into(),
    };
    runtime(client::dictate(options))
}

fn transcribe(dirs: &Dirs, args: &[String]) -> i32 {
    let Some(file) = args.first().filter(|a| !a.starts_with("--")) else {
        eprint!("{USAGE}");
        return 2;
    };
    let pcm = match std::fs::read(file).map_err(|e| e.to_string()).and_then(|b| audio::read_wav(&b)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{file}: {e}");
            return 1;
        }
    };
    let started = std::time::Instant::now();
    match service::transcribe_file(dirs, &pcm, value_of(args, "--engine"), value_of(args, "--model"), value_of(args, "--language")) {
        Ok((text, engine, model)) => {
            eprintln!("[{engine} · {model} · {:.2} s]", started.elapsed().as_secs_f64());
            println!("{text}");
            if text.trim().is_empty() { 3 } else { 0 }
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn status(args: &[String]) -> i32 {
    let json = args.iter().any(|a| a == "--json");
    runtime(async move {
        match client::status().await {
            Ok(map) => {
                if json {
                    println!("{}", serde_json::Value::Object(map));
                } else {
                    for (k, v) in &map {
                        println!("{k:<14} {}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()));
                    }
                }
                0
            }
            Err(e) => {
                eprintln!("{e}");
                4
            }
        }
    })
}

fn print_models(listing: &serde_json::Value) {
    for key in ["catalog", "installed"] {
        if let Some(items) = listing.get(key).and_then(|v| v.as_array()) {
            println!("{key}:");
            for item in items {
                println!("  {item}");
            }
        }
    }
}

fn model(dirs: &Dirs, args: &[String]) -> i32 {
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("download"), Some(name)) => {
            let Some(entry) = models::catalog_entry(name) else {
                eprintln!("{name}: not in the catalog");
                return 2;
            };
            match models::download(dirs, entry, true) {
                Ok(path) => {
                    println!("{}", path.display());
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        (Some("remove"), Some(name)) => match models::remove(dirs, name) {
            Ok(true) => 0,
            Ok(false) => {
                eprintln!("{name}: not installed by maprisma-voice");
                1
            }
            Err(e) => {
                eprintln!("{e}");
                2
            }
        },
        _ => {
            eprint!("{USAGE}");
            2
        }
    }
}

fn set(dirs: &Dirs, key: &str, value: Option<serde_json::Value>) -> i32 {
    if let Some(v) = &value {
        if let Err(e) = settings::check(key, v) {
            eprintln!("{key}: {e}");
            return 2;
        }
    }
    match settings::set(dirs, key, value) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}
