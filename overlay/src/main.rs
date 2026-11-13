// maprisma-dictation: the voice and typing overlay (docs/overlay.md).
//
//   maprisma-dictation [--toggle]   open, listening (ends on silence); open already → close
//   maprisma-dictation --hold       open for hold-to-talk (bind on key press) …
//   maprisma-dictation --release    … and this on key release: stop listening and ask
//   maprisma-dictation --demo       made-up voice levels, speech and answer (no mic, no helper)
//   maprisma-dictation --ask TEXT   open and ask TEXT at once, as if typed
//   maprisma-dictation --wake [--ask-rest TEXT]
//                                   the voice service heard the wake phrase: open and listen
//                                   (or ask TEXT); open already → listen again, never close

use maprisma_dictation::{instance, ui};

const USAGE: &str = "Usage: maprisma-dictation [--toggle] [--hold] [--release] [--demo] [--ask TEXT]
                          [--wake [--ask-rest TEXT]]

  --toggle    open the overlay and listen (the default); if it is open, close it
  --hold      hold-to-talk: listen until --release (a tap shorter than 250 ms keeps
              listening until you stop talking)
  --release   the talk key went up (bind it to the key's release)
  --demo      simulate a voice session and an answer (for trying the look)
  --ask TEXT  open without listening and ask TEXT at once (as if typed)
  --wake      started by the voice service when it heard the wake phrase: open and
              listen until you stop talking; if it is open already, listen again
  --ask-rest TEXT
              with --wake: what was said after the wake phrase; it is asked at once

Bind it to a key: see docs/overlay.md (COSMIC, KDE Plasma, Hyprland).";

fn main() -> cosmic::iced::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut flags = ui::Flags::default();
    let mut release = false;
    let mut wake = false;
    let mut rest: Option<String> = None;
    let mut args_iter = args.iter();
    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--ask" => match args_iter.next() {
                Some(question) if !question.trim().is_empty() => flags.ask = Some(question.clone()),
                _ => {
                    eprintln!("maprisma-dictation: --ask needs a question\n\n{USAGE}");
                    std::process::exit(2);
                }
            },
            "--toggle" => {}
            "--wake" => wake = true,
            "--ask-rest" => match args_iter.next() {
                Some(text) => rest = Some(text.chars().take(instance::MAX_REST).collect()),
                None => {
                    eprintln!("maprisma-dictation: --ask-rest needs a text\n\n{USAGE}");
                    std::process::exit(2);
                }
            },
            "--hold" => flags.hold = true,
            "--release" => release = true,
            "--demo" => flags.demo = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => {
                eprintln!("maprisma-dictation: unknown option {other}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    if rest.is_some() && !wake {
        eprintln!("maprisma-dictation: --ask-rest goes with --wake\n\n{USAGE}");
        std::process::exit(2);
    }
    let rest = rest.filter(|r| !r.trim().is_empty());
    if wake {
        flags.wake = true;
        flags.ask = rest.clone();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().expect("D-Bus runtime");
    if release {
        runtime.block_on(instance::release());
        return Ok(());
    }
    let connection = match runtime.block_on(instance::claim()) {
        Ok(Some(connection)) => connection,
        Ok(None) => {
            if wake {
                // The wake phrase never closes an open overlay: it listens again (or asks).
                runtime.block_on(instance::wake(rest.as_deref().unwrap_or("")));
            } else {
                // A second start toggles: the open overlay closes.
                runtime.block_on(instance::call("Toggle"));
            }
            return Ok(());
        }
        Err(e) => {
            eprintln!("maprisma-dictation: no session bus ({e})");
            std::process::exit(1);
        }
    };
    let result = cosmic::app::run::<ui::Overlay>(cosmic::app::Settings::default().no_main_window(true).transparent(true).exit_on_close(false), flags);
    ui::finish_helper_kills();
    drop(connection);
    result
}
