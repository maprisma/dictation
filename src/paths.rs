//! Where things live, and the few environment facts the service depends on. Everything goes
//! through a [`Dirs`] value, so tests build one on a temporary root and never touch the session's
//! files, PATH or keys (no global state, no `set_var`).

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const APP: &str = "maprisma-voice";

/// Environment variables the service reads besides the XDG folders.
pub const KEY_VARS: [&str; 2] = ["OPENAI_API_KEY", "GROQ_API_KEY"];

#[derive(Clone)]
pub struct Dirs {
    pub home: PathBuf,
    pub config_home: PathBuf,
    pub data_home: PathBuf,
    pub cache_home: PathBuf,
    /// `$XDG_RUNTIME_DIR`; temporary audio files go below it.
    pub runtime_dir: Option<PathBuf>,
    /// The PATH used to find programs.
    pub path_var: OsString,
    /// Only [KEY_VARS]; never printed (no Debug).
    keys: HashMap<String, String>,
}

impl std::fmt::Debug for Dirs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dirs")
            .field("home", &self.home)
            .field("config_home", &self.config_home)
            .field("data_home", &self.data_home)
            .field("runtime_dir", &self.runtime_dir)
            .finish_non_exhaustive()
    }
}

fn non_empty(var: &str) -> Option<OsString> {
    std::env::var_os(var).filter(|v| !v.is_empty())
}

impl Dirs {
    /// The session's folders and environment.
    pub fn from_env() -> Self {
        let home = PathBuf::from(non_empty("HOME").unwrap_or_else(|| "/".into()));
        let xdg = |var: &str, fallback: &str| non_empty(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(fallback));
        let keys = KEY_VARS
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
            .collect();
        Dirs {
            config_home: xdg("XDG_CONFIG_HOME", ".config"),
            data_home: xdg("XDG_DATA_HOME", ".local/share"),
            cache_home: xdg("XDG_CACHE_HOME", ".cache"),
            runtime_dir: non_empty("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute()),
            path_var: non_empty("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into()),
            home,
            keys,
        }
    }

    /// For tests: every folder below `root`, PATH as given, no keys.
    pub fn rooted(root: &Path, path_var: &str) -> Self {
        Dirs {
            home: root.join("home"),
            config_home: root.join("home/.config"),
            data_home: root.join("home/.local/share"),
            cache_home: root.join("home/.cache"),
            runtime_dir: Some(root.join("run")),
            path_var: path_var.into(),
            keys: HashMap::new(),
        }
    }

    /// For tests: pretend an environment variable is set.
    pub fn with_key(mut self, name: &str, value: &str) -> Self {
        self.keys.insert(name.into(), value.into());
        self
    }

    pub fn key(&self, name: &str) -> Option<&str> {
        self.keys.get(name).map(String::as_str)
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_home.join(APP).join("settings.json")
    }

    /// Our own data folder (`~/.local/share/maprisma-voice`).
    pub fn data_dir(&self) -> PathBuf {
        self.data_home.join(APP)
    }

    /// Where `model download` puts models.
    pub fn managed_models(&self) -> PathBuf {
        self.data_dir().join("models")
    }

    /// The whisper.cpp build the Manager installs.
    pub fn managed_server(&self) -> PathBuf {
        self.data_dir().join("whisper.cpp/bin/whisper-server")
    }

    /// Every folder searched for Whisper models, ours first (the ones Maprisma already uses).
    pub fn model_dirs(&self) -> Vec<PathBuf> {
        vec![
            self.managed_models(),
            self.data_home.join("maprisma/whisper/models"),
            self.data_home.join("voxtype/models"),
            self.cache_home.join("whisper"),
        ]
    }

    /// The first executable `name` on PATH (or `name` itself when it is a path).
    pub fn which(&self, name: &str) -> Option<PathBuf> {
        if name.contains('/') {
            let p = PathBuf::from(name);
            return is_executable(&p).then_some(p);
        }
        std::env::split_paths(&self.path_var)
            .map(|dir| dir.join(name))
            .find(|p| is_executable(p))
    }
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// Creates `dir` (and parents) and makes `dir` itself 0700.
pub fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Writes `bytes` to `path` through a temporary file in the same folder and a rename; the file
/// gets `mode`.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().ok_or_else(|| std::io::Error::other("no parent folder"))?;
    std::fs::create_dir_all(parent)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let temporary = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
pub mod testdir {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh empty folder for one test (removed when dropped).
    pub struct TestDir(pub PathBuf);

    impl TestDir {
        pub fn new(name: &str) -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!("maprisma-voice-test-{}-{}-{name}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TestDir(dir)
        }
        pub fn path(&self) -> &std::path::Path {
            &self.0
        }
        /// Writes an executable script `bin/<name>` and returns the bin folder.
        pub fn script(&self, name: &str, body: &str) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let bin = self.0.join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let path = bin.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            bin
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooted_paths_stay_in_the_root() {
        let d = Dirs::rooted(Path::new("/tmp/x"), "/nowhere");
        assert!(d.settings_file().starts_with("/tmp/x"));
        assert_eq!(d.settings_file(), PathBuf::from("/tmp/x/home/.config/maprisma-voice/settings.json"));
        assert!(d.model_dirs().iter().all(|p| p.starts_with("/tmp/x")));
        assert_eq!(d.model_dirs()[0], PathBuf::from("/tmp/x/home/.local/share/maprisma-voice/models"));
        assert_eq!(d.managed_server(), PathBuf::from("/tmp/x/home/.local/share/maprisma-voice/whisper.cpp/bin/whisper-server"));
        assert_eq!(d.key("OPENAI_API_KEY"), None);
        assert!(!format!("{:?}", d.clone().with_key("OPENAI_API_KEY", "sk-secret")).contains("sk-secret"));
    }

    #[test]
    fn which_finds_executables_only() {
        let t = testdir::TestDir::new("which");
        let bin = t.script("tool", "#!/bin/sh\n");
        std::fs::write(bin.join("plain"), "").unwrap();
        let d = Dirs::rooted(t.path(), bin.to_str().unwrap());
        assert_eq!(d.which("tool"), Some(bin.join("tool")));
        assert_eq!(d.which("plain"), None);
        assert_eq!(d.which("missing"), None);
        assert_eq!(d.which(bin.join("tool").to_str().unwrap()), Some(bin.join("tool")));
    }

    #[test]
    fn atomic_write_sets_mode() {
        use std::os::unix::fs::PermissionsExt;
        let t = testdir::TestDir::new("atomic");
        let p = t.path().join("a/b/file.json");
        write_atomic(&p, b"{}", 0o600).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"{}");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        private_dir(&t.path().join("a")).unwrap();
        assert_eq!(std::fs::metadata(t.path().join("a")).unwrap().permissions().mode() & 0o777, 0o700);
    }
}
