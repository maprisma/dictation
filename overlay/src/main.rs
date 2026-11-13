// maprisma-dictation: the voice and typing overlay (docs/overlay.md).
//
//   maprisma-dictation [--toggle]   open, listening (ends on silence); open already → close
//   maprisma-dictation --hold       open for hold-to-talk (bind on key press) …
//   maprisma-dictation --release    … and this on key release: stop listening and ask
//   maprisma-dictation --demo       made-up voice levels, speech and answer (no mic, no helper)

use maprisma_dictation::{instance, ui};

const USAGE: &str = "Usage: maprisma-dictation [--toggle] [--hold] [--release] [--demo]

  --toggle    open the overlay and listen (the default); if it is open, close it
  --hold      hold-to-talk: listen until --release (a tap shorter than 250 ms keeps
              listening until you stop talking)
  --release   the talk key went up (bind it to the key's release)
  --demo      simulate a voice session and an answer (for trying the look)

Bind it to a key: see docs/overlay.md (COSMIC, KDE Plasma, Hyprland).";

fn main() -> cosmic::iced::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut flags = ui::Flags::default();
    let mut release = false;
    for arg in &args {
        match arg.as_str() {
            "--toggle" => {}
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
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().expect("D-Bus runtime");
    if release {
        runtime.block_on(instance::release());
        return Ok(());
    }
    let connection = match runtime.block_on(instance::claim()) {
        Ok(Some(connection)) => connection,
        Ok(None) => {
            // A second start toggles: the open overlay closes.
            runtime.block_on(instance::call("Toggle"));
            return Ok(());
        }
        Err(e) => {
            eprintln!("maprisma-dictation: no session bus ({e})");
            std::process::exit(1);
        }
    };
    let result = cosmic::app::run::<ui::Overlay>(cosmic::app::Settings::default().no_main_window(true).transparent(true).exit_on_close(false), flags);
    drop(connection);
    result
}
