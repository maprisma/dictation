//! Recording: pw-record (else parecord, else arecord) as 16 kHz mono s16 raw on stdout, read in
//! 50 ms chunks into memory. Only the daemon records, and only while a session listens; the
//! recorder dies with the daemon (PDEATHSIG) and is killed when listening ends.

use crate::audio::CHUNK_SAMPLES;
use crate::paths::Dirs;
use crate::procs::ChildGuard;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorder {
    PwRecord(PathBuf),
    Parecord(PathBuf),
    Arecord(PathBuf),
}

pub fn find(dirs: &Dirs) -> Option<Recorder> {
    dirs.which("pw-record")
        .map(Recorder::PwRecord)
        .or_else(|| dirs.which("parecord").map(Recorder::Parecord))
        .or_else(|| dirs.which("arecord").map(Recorder::Arecord))
}

impl Recorder {
    pub fn name(&self) -> &'static str {
        match self {
            Recorder::PwRecord(_) => "pw-record",
            Recorder::Parecord(_) => "parecord",
            Recorder::Arecord(_) => "arecord",
        }
    }

    pub fn program(&self) -> &PathBuf {
        match self {
            Recorder::PwRecord(p) | Recorder::Parecord(p) | Recorder::Arecord(p) => p,
        }
    }

    /// Arguments for raw 16 kHz mono s16le on stdout from `microphone` (`default` = the
    /// desktop's default input).
    pub fn args(&self, microphone: &str) -> Vec<OsString> {
        let mic = (microphone != "default" && crate::settings::valid_microphone(microphone)).then_some(microphone);
        let mut args: Vec<String> = match self {
            Recorder::PwRecord(_) => {
                let mut a: Vec<String> = ["--raw", "--rate", "16000", "--channels", "1", "--format", "s16", "--latency", "50ms", "-P", "media.name=Maprisma Voice"].map(String::from).to_vec();
                if let Some(m) = mic {
                    a.extend(["--target".into(), m.into()]);
                }
                a.push("-".into());
                a
            }
            Recorder::Parecord(_) => {
                let mut a: Vec<String> = ["--raw", "--rate=16000", "--channels=1", "--format=s16le", "--latency-msec=50", "--client-name=Maprisma Voice"].map(String::from).to_vec();
                if let Some(m) = mic {
                    a.push(format!("--device={m}"));
                }
                a
            }
            Recorder::Arecord(_) => {
                let mut a: Vec<String> = ["-q", "-t", "raw", "-f", "S16_LE", "-r", "16000", "-c", "1"].map(String::from).to_vec();
                if let Some(m) = mic {
                    a.extend(["-D".into(), m.into()]);
                }
                a.push("-".into());
                a
            }
        };
        args.drain(..).map(OsString::from).collect()
    }
}

/// A running capture: chunks of samples until the recorder ends, and how to stop it.
pub struct Capture {
    pub chunks: tokio::sync::mpsc::Receiver<Vec<i16>>,
    pub stop: Box<dyn FnOnce() + Send>,
}

impl Capture {
    /// For tests: chunks from a list (then the stream ends, like a recorder that exits), or
    /// from a list followed by silence until stopped.
    pub fn from_samples(samples: Vec<i16>, then_silence: bool) -> Capture {
        let (tx, rx) = tokio::sync::mpsc::channel(4096);
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stopped.clone();
        std::thread::spawn(move || {
            for chunk in samples.chunks(CHUNK_SAMPLES) {
                if flag.load(std::sync::atomic::Ordering::SeqCst) || tx.blocking_send(chunk.to_vec()).is_err() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            while then_silence && !flag.load(std::sync::atomic::Ordering::SeqCst) {
                if tx.blocking_send(vec![0; CHUNK_SAMPLES]).is_err() {
                    return;
                }
                // Slightly faster than real time.
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });
        Capture { chunks: rx, stop: Box::new(move || stopped.store(true, std::sync::atomic::Ordering::SeqCst)) }
    }
}

pub fn start(recorder: &Recorder, microphone: &str) -> std::io::Result<Capture> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut command = Command::new(recorder.program());
    command.args(recorder.args(microphone)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = crate::procs::spawn(command)?;
    let mut stdout = child.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?;
    let guard = Arc::new(Mutex::new(ChildGuard(Some(child))));
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    std::thread::Builder::new().name("recorder".into()).spawn(move || {
        let mut buf = vec![0u8; CHUNK_SAMPLES * 2];
        loop {
            let mut filled = 0;
            while filled < buf.len() {
                match stdout.read(&mut buf[filled..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => filled += n,
                }
            }
            if filled >= 2 && tx.blocking_send(crate::audio::from_le_bytes(&buf[..filled])).is_err() {
                return;
            }
            if filled < buf.len() {
                return; // EOF: the recorder ended
            }
        }
    })?;
    let stopper = guard.clone();
    Ok(Capture {
        chunks: rx,
        stop: Box::new(move || {
            stopper.lock().unwrap_or_else(|e| e.into_inner()).kill();
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::testdir::TestDir;

    #[test]
    fn order_and_arguments() {
        let t = TestDir::new("recorders");
        let bin = t.script("arecord", "#!/bin/sh\n");
        let dirs = Dirs::rooted(t.path(), bin.to_str().unwrap());
        assert_eq!(find(&dirs).unwrap().name(), "arecord");
        t.script("parecord", "#!/bin/sh\n");
        assert_eq!(find(&dirs).unwrap().name(), "parecord");
        t.script("pw-record", "#!/bin/sh\n");
        assert_eq!(find(&dirs).unwrap().name(), "pw-record");
        let s = |r: Recorder, m: &str| r.args(m).iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
        let pw = s(Recorder::PwRecord("/p".into()), "default");
        assert!(pw.contains("--raw --rate 16000 --channels 1 --format s16") && pw.ends_with(" -") && !pw.contains("--target"));
        assert!(s(Recorder::PwRecord("/p".into()), "alsa_input.usb").contains("--target alsa_input.usb -"));
        assert!(s(Recorder::Parecord("/p".into()), "mic").contains("--device=mic"));
        assert!(s(Recorder::Arecord("/p".into()), "hw:1").contains("-D hw:1 -"));
        // An option-like name is never passed on.
        assert!(!s(Recorder::PwRecord("/p".into()), "--evil").contains("--evil"));
    }

    #[tokio::test]
    async fn reads_chunks_and_kills_on_stop() {
        let t = TestDir::new("recorder-run");
        // 0.25 s of samples, then hangs like a real recorder.
        let bin = t.script("pw-record", "#!/bin/sh\nhead -c 8000 /dev/zero\nexec sleep 30\n");
        let mut capture = start(&Recorder::PwRecord(bin.join("pw-record")), "default").unwrap();
        let mut got = 0;
        while got < 4000 {
            got += capture.chunks.recv().await.unwrap().len();
        }
        assert_eq!(got, 4000);
        (capture.stop)();
        // After the kill the stream ends.
        let end = tokio::time::timeout(std::time::Duration::from_secs(3), capture.chunks.recv()).await.unwrap();
        assert!(end.is_none());
    }
}
