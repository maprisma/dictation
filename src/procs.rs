//! Child processes. Every child gets `PR_SET_PDEATHSIG(SIGTERM)` so it dies with the service.
//!
//! Gotcha: the "parent" for PDEATHSIG is the *thread* that forked, not the process. Tokio's
//! blocking threads end when idle, which would kill a resident whisper-server ten seconds after
//! it was started. So every child is forked by one spawner thread that lives as long as the
//! process ([spawn]).

use std::process::{Child, Command, ExitStatus, Output};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

type Request = (Command, mpsc::Sender<std::io::Result<Child>>);

fn spawner() -> &'static Mutex<mpsc::Sender<Request>> {
    static SPAWNER: OnceLock<Mutex<mpsc::Sender<Request>>> = OnceLock::new();
    SPAWNER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Request>();
        std::thread::Builder::new()
            .name("spawner".into())
            .spawn(move || {
                for (mut command, reply) in rx {
                    let _ = reply.send(command.spawn());
                }
            })
            .expect("spawner thread");
        Mutex::new(tx)
    })
}

/// Starts `command` with PDEATHSIG from the long-lived spawner thread.
pub fn spawn(mut command: Command) -> std::io::Result<Child> {
    use std::os::unix::process::CommandExt;
    let parent = std::process::id() as libc::pid_t;
    // SAFETY: only async-signal-safe calls (prctl, getppid, _exit) between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // The parent died before prctl: don't outlive it.
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    let (reply, answer) = mpsc::channel();
    spawner().lock().unwrap_or_else(|e| e.into_inner()).send((command, reply)).map_err(|_| std::io::Error::other("spawner gone"))?;
    answer.recv().map_err(|_| std::io::Error::other("spawner gone"))?
}

/// A child that is killed (SIGTERM, then SIGKILL) and reaped when dropped.
pub struct ChildGuard(pub Option<Child>);

impl ChildGuard {
    pub fn pid(&self) -> Option<u32> {
        self.0.as_ref().map(Child::id)
    }
    /// Whether it still runs.
    pub fn alive(&mut self) -> bool {
        self.0.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }
    pub fn kill(&mut self) {
        if let Some(mut child) = self.0.take() {
            terminate(&mut child, Duration::from_millis(500));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// SIGTERM, wait up to `grace`, then SIGKILL; always reaps.
pub fn terminate(child: &mut Child, grace: Duration) {
    if let Ok(Some(_)) = child.try_wait() {
        return;
    }
    // SAFETY: plain kill(2) on our own child's pid.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let until = Instant::now() + grace;
    while Instant::now() < until {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub fn run_status(command: Command) -> std::io::Result<ExitStatus> {
    spawn(command)?.wait()
}

/// Runs a short command with a time limit; output is captured. Stdin is null unless set.
pub fn run_output(mut command: Command, timeout: Duration) -> std::io::Result<Output> {
    use std::process::Stdio;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = spawn(command)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let read = |pipe: Option<std::process::ChildStdout>| std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            use std::io::Read;
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let out = read(stdout);
    let err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = stderr {
            use std::io::Read;
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let until = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= until {
            terminate(&mut child, Duration::from_millis(300));
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok(Output { status, stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_and_timeout() {
        let mut c = Command::new("sh");
        c.args(["-c", "echo hi; echo err >&2; exit 3"]);
        let out = run_output(c, Duration::from_secs(5)).unwrap();
        assert_eq!(out.stdout, b"hi\n");
        assert_eq!(out.stderr, b"err\n");
        assert_eq!(out.status.code(), Some(3));
        let mut slow = Command::new("sleep");
        slow.arg("10");
        let started = Instant::now();
        assert_eq!(run_output(slow, Duration::from_millis(200)).unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn guard_kills_on_drop() {
        let mut c = Command::new("sleep");
        c.arg("30");
        let child = spawn(c).unwrap();
        let pid = child.id();
        drop(ChildGuard(Some(child)));
        // SAFETY: kill with signal 0 only checks whether the pid exists.
        assert_ne!(unsafe { libc::kill(pid as libc::pid_t, 0) }, 0);
    }

    #[test]
    fn spawned_from_a_short_thread_survives_it() {
        // PDEATHSIG belongs to the spawner thread, not to this short-lived one.
        let child = std::thread::spawn(|| {
            let mut c = Command::new("sleep");
            c.arg("30");
            spawn(c).unwrap()
        })
        .join()
        .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let mut guard = ChildGuard(Some(child));
        assert!(guard.alive());
        guard.kill();
        assert!(!guard.alive());
    }
}
