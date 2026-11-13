//! Where the overlay's files live:
//! - `$XDG_CONFIG_HOME/maprisma-dictation/settings.json`: its settings (read only here; Maprisma
//!   Manager's Voice page writes them, see docs/overlay.md);
//! - `$XDG_CONFIG_HOME/maprisma-ai/settings.json`: the AI helper all Maprisma apps share;
//! - `$XDG_RUNTIME_DIR/maprisma-dictation/`: the last exchange (for follow-ups within 45 s) and
//!   the helper's work folder, both private (0700/0600) and gone at logout.
//!
//! Tests point every root at a temporary folder, so they never read or write the session's files.

use std::path::PathBuf;
use std::sync::OnceLock;

pub const DIR: &str = "maprisma-dictation";

/// For tests: one temporary root per test process.
pub fn test_roots() -> PathBuf {
    static DIR_ONCE: OnceLock<PathBuf> = OnceLock::new();
    DIR_ONCE
        .get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("maprisma-dictation-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for sub in ["config", "runtime"] {
                std::fs::create_dir_all(dir.join(sub)).unwrap();
            }
            dir
        })
        .clone()
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var).map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => home().join(fallback),
    }
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

pub fn config_root() -> PathBuf {
    if cfg!(test) {
        return test_roots().join("config");
    }
    xdg("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_RUNTIME_DIR` (else a folder in the cache) — private to this user.
pub fn runtime_root() -> PathBuf {
    if cfg!(test) {
        return test_roots().join("runtime");
    }
    match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => xdg("XDG_CACHE_HOME", ".cache"),
    }
}

pub fn settings_file() -> PathBuf {
    config_root().join(DIR).join("settings.json")
}

/// The private runtime folder (made 0700 when missing).
pub fn runtime_dir() -> std::io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let dir = runtime_root().join(DIR);
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = std::fs::symlink_metadata(&dir)?;
    // Only a real folder of ours that nobody else can write to.
    if !meta.is_dir() || meta.uid() != unsafe { libc::getuid() } || meta.permissions().mode() & 0o022 != 0 {
        return Err(std::io::Error::other("the runtime folder is not private"));
    }
    Ok(dir)
}
