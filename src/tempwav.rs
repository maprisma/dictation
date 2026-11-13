//! The only place audio touches the disk: a 0600 WAV in a 0700 folder under `$XDG_RUNTIME_DIR`
//! (a tmpfs), for an engine that needs a file. It is removed when the [TempWav] is dropped.

use crate::paths::Dirs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct TempWav(PathBuf);

impl TempWav {
    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn create(dirs: &Dirs, bytes: &[u8]) -> std::io::Result<TempWav> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = folder(dirs)?;
        let path = dir.join(format!("utterance-{}-{}.wav", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
        let guard = TempWav(path);
        file.write_all(bytes)?;
        file.flush()?;
        Ok(guard)
    }
}

impl Drop for TempWav {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `$XDG_RUNTIME_DIR/maprisma-voice` (else `/tmp/maprisma-voice-<uid>`), 0700, ours, no symlink.
fn folder(dirs: &Dirs) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    // SAFETY: getuid never fails.
    let uid = unsafe { libc::getuid() };
    let dir = match &dirs.runtime_dir {
        Some(run) => run.join(crate::paths::APP),
        None => std::env::temp_dir().join(format!("{}-{uid}", crate::paths::APP)),
    };
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::create_dir(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = std::fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != uid {
        return Err(std::io::Error::other(format!("{} is not a private folder", dir.display())));
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::testdir::TestDir;

    #[test]
    fn private_and_removed() {
        use std::os::unix::fs::PermissionsExt;
        let t = TestDir::new("tempwav");
        let dirs = Dirs::rooted(t.path(), "/nowhere");
        let wav = TempWav::create(&dirs, b"RIFF").unwrap();
        let path = wav.path().to_path_buf();
        assert!(path.starts_with(t.path().join("run/maprisma-voice")));
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        drop(wav);
        assert!(!path.exists());
    }

    #[test]
    fn refuses_a_symlinked_folder() {
        let t = TestDir::new("tempwav-link");
        std::fs::create_dir_all(t.path().join("run")).unwrap();
        std::fs::create_dir_all(t.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(t.path().join("elsewhere"), t.path().join("run/maprisma-voice")).unwrap();
        let dirs = Dirs::rooted(t.path(), "/nowhere");
        assert!(TempWav::create(&dirs, b"RIFF").is_err());
    }
}
