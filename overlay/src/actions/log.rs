//! The action log: `$XDG_STATE_HOME/maprisma-dictation/actions.jsonl` (folder 0700, file 0600),
//! one JSON object per executed, undone, blocked, cancelled or failed action:
//! `{"time": unix seconds, "request": the question (cut at 300 characters), "action": {…},
//! "level": 0|1|2, "result": "done"|"failed"|"blocked"|"cancelled"|"undone"|"unsupported"|"rejected",
//! "detail": short text}`. At 1 MB it moves to `actions.jsonl.1` (the older one goes).

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const FILE: &str = "actions.jsonl";
pub const ROTATE_AT: u64 = 1024 * 1024;
const REQUEST_MAX: usize = 300;
const DETAIL_MAX: usize = 500;

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub time: u64,
    pub request: String,
    pub action: Value,
    pub level: u8,
    pub result: &'static str,
    pub detail: String,
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max).collect::<String>()) }
}

pub fn line(e: &Entry) -> String {
    json!({"time": e.time, "request": cut(&e.request, REQUEST_MAX), "action": e.action, "level": e.level, "result": e.result, "detail": cut(&e.detail, DETAIL_MAX)}).to_string()
}

/// The private folder (made 0700; refused when it isn't ours or others can write to it).
pub fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::getuid() } || meta.permissions().mode() & 0o022 != 0 {
        return Err(std::io::Error::other("the log folder is not private"));
    }
    Ok(())
}

pub fn append_in(dir: &Path, e: &Entry) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    private_dir(dir)?;
    let path = dir.join(FILE);
    let text = line(e) + "\n";
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !meta.is_file() {
            return Err(std::io::Error::other("the log is not a file"));
        }
        if meta.len() + text.len() as u64 > ROTATE_AT {
            std::fs::rename(&path, dir.join(format!("{FILE}.1")))?;
        }
    }
    let mut f = std::fs::OpenOptions::new().append(true).create(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&path)?;
    f.write_all(text.as_bytes())
}

pub fn dir() -> PathBuf {
    crate::paths::state_root().join(crate::paths::DIR)
}

/// Appends; a log that can't be written never stops an action (it says so on stderr).
pub fn append(e: &Entry) {
    if let Err(err) = append_in(&dir(), e) {
        eprintln!("maprisma-dictation: the action log could not be written ({err})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_and_rotating() {
        use std::os::unix::fs::PermissionsExt;
        let d = crate::paths::test_roots().join("state-log");
        let _ = std::fs::remove_dir_all(&d);
        let e = Entry { time: 7, request: "x".repeat(400), action: json!({"kind": "dnd", "on": true}), level: 0, result: "done", detail: "on".into() };
        append_in(&d, &e).unwrap();
        append_in(&d, &e).unwrap();
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(d.join(FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        let text = std::fs::read_to_string(d.join(FILE)).unwrap();
        assert_eq!(text.lines().count(), 2);
        let v: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(v["action"]["kind"], "dnd");
        assert_eq!(v["result"], "done");
        assert_eq!(v["request"].as_str().unwrap().chars().count(), REQUEST_MAX + 1);
        // fill it to the limit: the next line rotates
        let big = Entry { detail: "y".repeat(DETAIL_MAX), ..e.clone() };
        let per = line(&big).len() as u64 + 1;
        let mut size = std::fs::metadata(d.join(FILE)).unwrap().len();
        while size + per <= ROTATE_AT {
            append_in(&d, &big).unwrap();
            size += per;
        }
        append_in(&d, &big).unwrap();
        assert!(d.join(format!("{FILE}.1")).exists());
        assert_eq!(std::fs::read_to_string(d.join(FILE)).unwrap().lines().count(), 1);
        assert!(std::fs::metadata(d.join(format!("{FILE}.1"))).unwrap().len() <= ROTATE_AT);
        // a folder others can write to is refused
        let open = crate::paths::test_roots().join("state-open");
        std::fs::create_dir_all(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(append_in(&open, &e).is_err());
    }
}
