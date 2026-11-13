//! Stopping a whole process group: what a helper or a `run_command` started in the background
//! must not outlive it. SIGTERM to the group, a grace period, then SIGKILL to every member that
//! is still there. Members are identified by pid *and* start time (from `/proc`), so a reused pid
//! is never hit. A process that left the group (`setsid`) can't be found this way.

use std::time::{Duration, Instant};

/// (pid, start time) of every live process in group `pgid` (zombies left out).
pub fn members(pgid: i32) -> Vec<(i32, u64)> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return vec![] };
    dir.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter_map(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let (group, start, state) = parse_stat(&stat)?;
            (group == pgid && state != 'Z' && state != 'X').then_some((pid, start))
        })
        .collect()
}

/// (pgrp, starttime, state) from a `/proc/<pid>/stat` line.
fn parse_stat(stat: &str) -> Option<(i32, u64, char)> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // after the name: state(3) ppid(4) pgrp(5) … starttime(22)
    Some((fields.get(2)?.parse().ok()?, fields.get(19)?.parse().ok()?, fields.first()?.chars().next()?))
}

fn still_there(pid: i32, start: u64) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).ok().and_then(|s| parse_stat(&s)).is_some_and(|(_, st, state)| st == start && state != 'Z' && state != 'X')
}

/// SIGTERM to group `pgid`, wait up to `grace` for its members, then SIGKILL the ones left.
/// Blocking. Returns how many had to be killed.
pub fn terminate(pgid: i32, grace: Duration) -> usize {
    if pgid <= 1 {
        return 0;
    }
    let before = members(pgid);
    if before.is_empty() {
        return 0;
    }
    // SAFETY: kill(2) on a process group we created (its leader's pid).
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    let until = Instant::now() + grace;
    loop {
        let left: Vec<_> = before.iter().filter(|(pid, start)| still_there(*pid, *start)).collect();
        if left.is_empty() {
            return 0;
        }
        if Instant::now() >= until {
            for (pid, _) in &left {
                // SAFETY: plain kill(2) on a pid just checked to be the same process.
                unsafe {
                    libc::kill(*pid, libc::SIGKILL);
                }
            }
            return left.len();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    #[test]
    fn stat_lines() {
        let line = "1234 (a (b) c) S 1 1234 1234 0 -1 4194560 100 0 0 0 0 0 0 0 20 0 1 0 98765 1000 10";
        assert_eq!(parse_stat(line), Some((1234, 98765, 'S')));
        assert_eq!(parse_stat("garbage"), None);
    }

    /// A member that ignores SIGTERM is killed after the grace period.
    #[test]
    fn term_then_kill() {
        let mut child = std::process::Command::new("sh").args(["-c", "trap '' TERM; sleep 30 & sleep 30; wait"]).process_group(0).spawn().unwrap();
        let pgid = child.id() as i32;
        let until = Instant::now() + Duration::from_secs(3);
        while members(pgid).len() < 3 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(members(pgid).len(), 3);
        let started = Instant::now();
        assert!(terminate(pgid, Duration::from_millis(300)) >= 1);
        assert!(started.elapsed() < Duration::from_secs(2));
        let _ = child.wait();
        let until = Instant::now() + Duration::from_secs(2);
        while !members(pgid).is_empty() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(members(pgid).is_empty());
        assert_eq!(terminate(pgid, Duration::from_millis(100)), 0);
    }
}
