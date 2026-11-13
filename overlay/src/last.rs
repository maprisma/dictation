//! The last exchange, kept for follow-ups when the overlay is opened again within 45 s:
//! `$XDG_RUNTIME_DIR/maprisma-dictation/last.json` (0600, gone at logout, removed when stale).

use crate::helper::{Exchange, FOLLOW_UP_SECS, follow_up};
use serde_json::{Value, json};
use std::path::Path;

const FILE: &str = "last.json";
const FILE_MAX: u64 = 512 * 1024;

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn save_in(dir: &Path, e: &Exchange) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = dir.join(format!(".{FILE}.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    f.write_all(json!({"question": e.question, "answer": e.answer, "at": e.at}).to_string().as_bytes())?;
    drop(f);
    std::fs::rename(&tmp, dir.join(FILE))
}

pub fn load_in(dir: &Path, now: u64) -> Option<Exchange> {
    use std::io::Read;
    let path = dir.join(FILE);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > FILE_MAX {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(&path).ok()?.take(FILE_MAX).read_to_string(&mut text).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let e = Exchange { question: v["question"].as_str()?.into(), answer: v["answer"].as_str()?.into(), at: v["at"].as_u64()? };
    let fresh = follow_up(Some(&e), now).is_some();
    if !fresh && now > e.at + FOLLOW_UP_SECS {
        let _ = std::fs::remove_file(&path);
    }
    fresh.then_some(e)
}

pub fn save(e: &Exchange) {
    if let Ok(dir) = crate::paths::runtime_dir() {
        let _ = save_in(&dir, e);
    }
}

pub fn load() -> Option<Exchange> {
    load_in(&crate::paths::runtime_dir().ok()?, now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_for_45_seconds() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::paths::runtime_dir().unwrap();
        assert!(dir.starts_with(crate::paths::test_roots()));
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let e = Exchange { question: "q".into(), answer: "a\n\"b\"".into(), at: 5000 };
        save_in(&dir, &e).unwrap();
        assert_eq!(std::fs::metadata(dir.join(FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load_in(&dir, 5040), Some(e.clone()));
        assert_eq!(load_in(&dir, 5046), None);
        assert!(!dir.join(FILE).exists(), "stale: removed");
        std::fs::write(dir.join(FILE), "{broken").unwrap();
        assert_eq!(load_in(&dir, 5000), None);
    }
}
