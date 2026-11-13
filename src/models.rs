//! Whisper models: the starter catalog (the same three files and URLs as Maprisma's
//! `dictation.dart`, plus their size and SHA-256), which files count as Whisper models, where
//! they are searched, which one is used, and the verified, atomic download.

use crate::paths::Dirs;
use regex::Regex;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModel {
    pub id: &'static str,
    pub file: &'static str,
    pub label: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    pub url: &'static str,
}

/// From huggingface.co/ggerganov/whisper.cpp (LFS size and oid, 2026-11-13).
pub const CATALOG: [CatalogModel; 3] = [
    CatalogModel {
        id: "base",
        file: "ggml-base.bin",
        label: "Whisper base (148 MB · fast, any PC)",
        size: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
    },
    CatalogModel {
        id: "small",
        file: "ggml-small.bin",
        label: "Whisper small (488 MB · balanced)",
        size: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
    },
    CatalogModel {
        id: "turbo",
        file: "ggml-large-v3-turbo-q5_0.bin",
        label: "Whisper large-v3-turbo (574 MB · best accuracy)",
        size: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
    },
];

static WHISPER_GGML: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^ggml-(tiny|base|small|medium|large(-v[123])?(-turbo)?)(\.en)?(-q[0-9]_[0-9k])?\.bin$").unwrap()
});
static SAFE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._-]+\.(bin|gguf)$").unwrap());

/// Only real Whisper GGML/GGUF files count as Whisper models, never general `ggml-*.bin` LLM
/// weights or `.part` downloads (Maprisma's `isWhisperModelFile`).
pub fn is_whisper_model_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    if lower.ends_with(".part") {
        return false;
    }
    WHISPER_GGML.is_match(&lower) || (lower.contains("whisper") && (lower.ends_with(".bin") || lower.ends_with(".gguf")))
}

/// A plain file name that is safe to join to a folder (Maprisma's remove check).
pub fn valid_file_name(name: &str) -> bool {
    !name.contains('/') && !name.contains('\\') && !name.contains("..") && SAFE_NAME.is_match(name)
}

/// The `model` setting: a catalog id or a safe Whisper model file name.
pub fn valid_setting(value: &str) -> bool {
    CATALOG.iter().any(|m| m.id == value) || (valid_file_name(value) && is_whisper_model_file(value))
}

pub fn catalog_entry(name: &str) -> Option<&'static CatalogModel> {
    CATALOG.iter().find(|m| m.id == name || m.file == name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    /// In our own folder (`model download` / `model remove` manage it).
    pub managed: bool,
}

/// Every Whisper model in [Dirs::model_dirs], sorted by name; the first folder wins for a name
/// found twice. Symlinks to files count, folders and other entries don't.
pub fn discover(dirs: &Dirs) -> Vec<ModelFile> {
    let managed = dirs.managed_models();
    let mut found: Vec<ModelFile> = Vec::new();
    for dir in dirs.model_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            if !is_whisper_model_file(&name) || found.iter().any(|m| m.name == name) {
                continue;
            }
            let path = entry.path();
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if !meta.is_file() || meta.len() == 0 {
                continue;
            }
            found.push(ModelFile { name, path, size: meta.len(), managed: dir == managed });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// The model to use: the setting (id or file) when it is there, else large-v3-turbo*, then
/// small*, then base*, then the first one.
pub fn pick<'a>(models: &'a [ModelFile], setting: &str) -> Option<&'a ModelFile> {
    if !setting.is_empty() {
        let file = catalog_entry(setting).map_or(setting, |m| m.file);
        if let Some(m) = models.iter().find(|m| m.name == file) {
            return Some(m);
        }
    }
    let starts = |prefix: &str| {
        let mut list: Vec<&ModelFile> = models.iter().filter(|m| m.name.starts_with(prefix)).collect();
        // The exact starter file first, then the rest by name.
        list.sort_by_key(|m| (!CATALOG.iter().any(|c| c.file == m.name), m.name.clone()));
        list.first().copied()
    };
    starts("ggml-large-v3-turbo")
        .or_else(|| models.iter().find(|m| m.name.contains("large-v3-turbo")))
        .or_else(|| starts("ggml-small"))
        .or_else(|| starts("ggml-base"))
        .or_else(|| models.first())
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Whether `path` has the catalog entry's size and hash.
pub fn verify(path: &Path, model: &CatalogModel) -> std::io::Result<bool> {
    let meta = std::fs::metadata(path)?;
    Ok(meta.len() == model.size && sha256_file(path)? == model.sha256)
}

/// Downloads a catalog model into [Dirs::managed_models] with curl (to `<file>.part`), checks
/// size and SHA-256 and renames it into place. An already present and valid file is kept.
/// Returns the model's path.
pub fn download(dirs: &Dirs, model: &CatalogModel, progress: bool) -> Result<PathBuf, String> {
    let dir = dirs.managed_models();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dest = dir.join(model.file);
    if dest.is_file() && verify(&dest, model).unwrap_or(false) {
        return Ok(dest);
    }
    let curl = dirs.which("curl").ok_or("curl is not installed")?;
    let part = dir.join(format!("{}.part", model.file));
    let _ = std::fs::remove_file(&part);
    let mut command = std::process::Command::new(curl);
    command.args(["-fL", "--retry", "2", "--connect-timeout", "20"]);
    command.arg(if progress { "--progress-bar" } else { "-sS" });
    command.arg("-o").arg(&part).arg(model.url);
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null());
    let status = crate::procs::run_status(command).map_err(|e| format!("curl: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("the download failed ({status})"));
    }
    match verify(&part, model) {
        Ok(true) => {}
        Ok(false) => {
            let _ = std::fs::remove_file(&part);
            return Err("the downloaded file has the wrong size or checksum".into());
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e.to_string());
        }
    }
    std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Removes a model (and a leftover `.part`) from our own folder only.
pub fn remove(dirs: &Dirs, name: &str) -> Result<bool, String> {
    let file = catalog_entry(name).map_or(name, |m| m.file);
    if !valid_file_name(file) {
        return Err("invalid model name".into());
    }
    let mut removed = false;
    for path in [dirs.managed_models().join(file), dirs.managed_models().join(format!("{file}.part"))] {
        if std::fs::symlink_metadata(&path).map(|m| m.is_file() || m.file_type().is_symlink()).unwrap_or(false) {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            removed = true;
        }
    }
    Ok(removed)
}

/// The `models` command's JSON: the catalog with what is installed, every model found, and the
/// one in use.
pub fn listing(dirs: &Dirs, setting: &str) -> Value {
    let models = discover(dirs);
    let active = pick(&models, setting);
    json!({
        "catalog": CATALOG.iter().map(|c| {
            let installed = models.iter().find(|m| m.name == c.file);
            json!({"id": c.id, "file": c.file, "label": c.label, "size": c.size, "sha256": c.sha256, "url": c.url,
                "installed": installed.is_some(), "path": installed.map(|m| m.path.display().to_string())})
        }).collect::<Vec<_>>(),
        "models": models.iter().map(|m| json!({"name": m.name, "path": m.path.display().to_string(), "size": m.size,
            "managed": m.managed, "active": active.is_some_and(|a| a.path == m.path)})).collect::<Vec<_>>(),
        "active": active.map(|m| m.name.clone()),
        "folder": dirs.managed_models().display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::testdir::TestDir;

    #[test]
    fn whisper_model_files() {
        for ok in ["ggml-base.bin", "ggml-small.en.bin", "ggml-large-v3-turbo-q5_0.bin", "ggml-large-v3.bin", "ggml-medium-q8_0.bin",
            "GGML-TINY.BIN", "my-whisper-de.gguf", "whisper-large.bin"] {
            assert!(is_whisper_model_file(ok), "{ok}");
        }
        for bad in ["ggml-model-q4_0.bin", "llama-3.gguf", "ggml-base.bin.part", "ggml-huge.bin", "notes.txt", "whisper.txt"] {
            assert!(!is_whisper_model_file(bad), "{bad}");
        }
    }

    #[test]
    fn file_names_and_settings() {
        for ok in ["ggml-base.bin", "a_b-c.gguf", "x.y.bin"] {
            assert!(valid_file_name(ok), "{ok}");
        }
        for bad in ["../ggml-base.bin", "a/b.bin", "a\\b.bin", "ggml base.bin", "x.txt", "", ".bin", "x..bin", "ggml-base.bin\n"] {
            assert!(!valid_file_name(bad), "{bad:?}");
        }
        assert!(valid_setting("turbo") && valid_setting("ggml-small.bin"));
        assert!(!valid_setting("ggml-model-q4_0.bin") && !valid_setting("/x/ggml-base.bin"));
        assert_eq!(catalog_entry("small").unwrap().file, "ggml-small.bin");
        assert_eq!(catalog_entry("ggml-base.bin").unwrap().id, "base");
        // The catalog's URLs are the ones Maprisma uses, and each names its file.
        for c in &CATALOG {
            assert!(c.url.starts_with("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/"));
            assert!(c.url.ends_with(c.file) && is_whisper_model_file(c.file) && c.sha256.len() == 64);
        }
    }

    fn model(name: &str) -> ModelFile {
        ModelFile { name: name.into(), path: PathBuf::from("/m").join(name), size: 1, managed: true }
    }

    #[test]
    fn picking() {
        let all = vec![model("ggml-base.bin"), model("ggml-large-v3-turbo-q5_0.bin"), model("ggml-small.bin")];
        assert_eq!(pick(&all, "").unwrap().name, "ggml-large-v3-turbo-q5_0.bin");
        assert_eq!(pick(&all, "base").unwrap().name, "ggml-base.bin");
        assert_eq!(pick(&all, "ggml-small.bin").unwrap().name, "ggml-small.bin");
        // A setting that isn't installed falls back to the preference.
        assert_eq!(pick(&all, "ggml-medium.bin").unwrap().name, "ggml-large-v3-turbo-q5_0.bin");
        let two = vec![model("ggml-base.bin"), model("ggml-small.en.bin")];
        assert_eq!(pick(&two, "").unwrap().name, "ggml-small.en.bin");
        let turbo = vec![model("ggml-large-v3-turbo.bin"), model("ggml-large-v3-turbo-q5_0.bin")];
        assert_eq!(pick(&turbo, "").unwrap().name, "ggml-large-v3-turbo-q5_0.bin");
        let other = vec![model("ggml-medium.bin"), model("whisper-x.gguf")];
        assert_eq!(pick(&other, "").unwrap().name, "ggml-medium.bin");
        assert_eq!(pick(&[], ""), None);
    }

    #[test]
    fn discovery_in_all_folders() {
        let t = TestDir::new("models");
        let dirs = Dirs::rooted(t.path(), "/nowhere");
        let folders = dirs.model_dirs();
        for f in &folders {
            std::fs::create_dir_all(f).unwrap();
        }
        std::fs::write(folders[0].join("ggml-base.bin"), "x").unwrap();
        std::fs::write(folders[0].join("ggml-small.bin.part"), "x").unwrap();
        std::fs::write(folders[1].join("ggml-base.bin"), "dup").unwrap();
        std::fs::write(folders[2].join("ggml-small.bin"), "x").unwrap();
        std::fs::write(folders[3].join("ggml-model-q4_0.bin"), "llm").unwrap();
        std::fs::write(folders[3].join("ggml-tiny.bin"), "").unwrap(); // empty: skipped
        std::fs::create_dir_all(folders[3].join("ggml-medium.bin")).unwrap(); // a folder: skipped
        let found = discover(&dirs);
        let names: Vec<&str> = found.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["ggml-base.bin", "ggml-small.bin"]);
        assert!(found[0].managed && found[0].path.starts_with(&folders[0]));
        assert!(!found[1].managed);
        let listing = listing(&dirs, "");
        assert_eq!(listing["active"], "ggml-small.bin");
        assert_eq!(listing["catalog"][0]["installed"], true);
        assert_eq!(listing["catalog"][2]["installed"], false);
    }

    fn fake_catalog(content: &[u8]) -> CatalogModel {
        use sha2::{Digest, Sha256};
        let sha: &'static str = Box::leak(format!("{:x}", Sha256::digest(content)).into_boxed_str());
        CatalogModel { id: "fake", file: "ggml-tiny.bin", label: "fake", size: content.len() as u64, sha256: sha, url: "https://example.invalid/ggml-tiny.bin" }
    }

    /// A fake curl that writes $FAKE_CONTENT (or fails) to the -o file, no network.
    fn fake_curl(t: &TestDir, content: &str, exit: i32) -> String {
        let body = format!("#!/bin/sh\nout=\nwhile [ $# -gt 0 ]; do [ \"$1\" = -o ] && out=$2; shift; done\nprintf '%s' '{content}' > \"$out\"\nexit {exit}\n");
        t.script("curl", &body).display().to_string()
    }

    #[test]
    fn download_verifies_and_is_atomic() {
        let t = TestDir::new("download");
        let path = fake_curl(&t, "model-bytes", 0);
        let dirs = Dirs::rooted(t.path(), &path);
        let good = fake_catalog(b"model-bytes");
        let dest = download(&dirs, &good, false).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"model-bytes");
        assert!(!dirs.managed_models().join("ggml-tiny.bin.part").exists());
        // Wrong checksum: nothing installed, no .part left behind.
        std::fs::remove_file(&dest).unwrap();
        let bad = fake_catalog(b"other-bytes!");
        assert!(download(&dirs, &bad, false).is_err());
        assert!(!dest.exists() && !dirs.managed_models().join("ggml-tiny.bin.part").exists());
        // curl fails.
        let t2 = TestDir::new("download-fail");
        let dirs2 = Dirs::rooted(t2.path(), &fake_curl(&t2, "x", 22));
        assert!(download(&dirs2, &good, false).unwrap_err().contains("failed"));
        // remove: only our folder, only valid names.
        download(&dirs, &good, false).unwrap();
        assert_eq!(remove(&dirs, "ggml-tiny.bin"), Ok(true));
        assert_eq!(remove(&dirs, "ggml-tiny.bin"), Ok(false));
        assert!(remove(&dirs, "../x.bin").is_err());
    }

    #[test]
    fn sha_of_a_file() {
        let t = TestDir::new("sha");
        let p = t.path().join("f");
        std::fs::write(&p, "abc").unwrap();
        assert_eq!(sha256_file(&p).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
