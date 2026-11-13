//! Engines that record and type the text themselves (as in Omarchy): Voxtype
//! (`voxtype record start|stop|cancel`), Superwhisper (`superwhisper start|stop`) and Omarchy
//! (`omarchy-dictation start|stop`, which runs its configured backend). The service never opens
//! the microphone for them; their session's `Final` has empty text and `typed = true`.
//! Superwhisper and Omarchy can't cancel: a cancel stops them like Finish (they may still type).

use super::Engine;
use crate::VoiceError;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Voxtype,
    Superwhisper,
    Omarchy,
}

impl Kind {
    pub fn id(self) -> &'static str {
        match self {
            Kind::Voxtype => "voxtype",
            Kind::Superwhisper => "superwhisper",
            Kind::Omarchy => "omarchy",
        }
    }
    pub fn missing(self) -> &'static str {
        match self {
            Kind::Voxtype => "no_voxtype",
            Kind::Superwhisper => "no_superwhisper",
            Kind::Omarchy => "no_omarchy",
        }
    }
    pub fn start_args(self) -> &'static [&'static str] {
        match self {
            Kind::Voxtype => &["record", "start"],
            _ => &["start"],
        }
    }
    pub fn stop_args(self, cancel: bool) -> &'static [&'static str] {
        match (self, cancel) {
            (Kind::Voxtype, true) => &["record", "cancel"],
            (Kind::Voxtype, false) => &["record", "stop"],
            _ => &["stop"],
        }
    }
}

pub struct ExternalEngine {
    pub kind: Kind,
    pub program: PathBuf,
}

impl ExternalEngine {
    fn run(&self, args: &[&str]) -> Result<(), VoiceError> {
        let mut command = std::process::Command::new(&self.program);
        command.args(args).stdin(std::process::Stdio::null());
        let output = crate::procs::run_output(command, Duration::from_secs(15)).map_err(|e| VoiceError::engine(format!("{}: {e}", self.kind.id())))?;
        if output.status.success() {
            Ok(())
        } else {
            let detail = String::from_utf8_lossy(&output.stderr).trim().lines().last().unwrap_or("").to_string();
            Err(VoiceError::engine(format!("{} {} failed ({}) {detail}", self.kind.id(), args.join(" "), output.status).trim().to_string()))
        }
    }
}

impl Engine for ExternalEngine {
    fn id(&self) -> &'static str {
        self.kind.id()
    }
    fn external(&self) -> bool {
        true
    }
    fn start(&self) -> Result<(), VoiceError> {
        self.run(self.kind.start_args())
    }
    fn stop(&self, cancel: bool) -> Result<(), VoiceError> {
        self.run(self.kind.stop_args(cancel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{Dirs, testdir::TestDir};

    #[test]
    fn fake_programs_on_path() {
        let t = TestDir::new("external");
        let log = t.path().join("log");
        for name in ["voxtype", "superwhisper", "omarchy-dictation"] {
            t.script(name, &format!("#!/bin/sh\necho \"{name} $*\" >> {}\n", log.display()));
        }
        let bin = t.script("broken", "#!/bin/sh\necho 'no daemon running' >&2\nexit 2\n");
        let dirs = Dirs::rooted(t.path(), bin.to_str().unwrap());
        let facts = super::super::probe(&dirs, &crate::settings::Settings::default());
        // No recorder on this PATH: auto still finds the external ones, Omarchy first.
        assert_eq!(super::super::choose("auto", &facts).unwrap().id(), "omarchy");
        for (kind, name) in [(Kind::Voxtype, "voxtype"), (Kind::Superwhisper, "superwhisper"), (Kind::Omarchy, "omarchy-dictation")] {
            let e = ExternalEngine { kind, program: dirs.which(name).unwrap() };
            assert!(e.external() && !e.partials());
            e.start().unwrap();
            e.stop(false).unwrap();
            e.stop(true).unwrap();
            assert!(e.transcribe(&[0; 10], "auto").is_err());
        }
        let log = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            log.lines().collect::<Vec<_>>(),
            ["voxtype record start", "voxtype record stop", "voxtype record cancel", "superwhisper start", "superwhisper stop", "superwhisper stop",
             "omarchy-dictation start", "omarchy-dictation stop", "omarchy-dictation stop"]
        );
        let broken = ExternalEngine { kind: Kind::Voxtype, program: bin.join("broken") };
        let err = broken.start().unwrap_err();
        assert!(err.message.contains("no daemon running"), "{}", err.message);
    }
}
