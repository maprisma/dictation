//! Asking the shared AI helper (Maprisma Manager → AI) one question.
//!
//! The helpers come from Maprisma's catalog (`/usr/lib/maprisma/data/helpers.json` when it parses
//! and is at least as new as the copy shipped here, `data/helpers.json`), the same file the
//! Launcher, Theme and Notifier read. Only entries with an `ask` (one prompt in, one answer on
//! stdout, no tools or read-only) whose program is installed count. The prompt goes into the
//! argv as one element (never through a shell) or on stdin when the entry says so; the run is
//! bounded in time and output and killed when the overlay cancels it (Esc) or closes.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const SYSTEM_CATALOG: &str = "/usr/lib/maprisma/data/helpers.json";
pub const SHIPPED: &str = include_str!("../data/helpers.json");
pub const PROMPT: &str = "{prompt}";
const CATALOG_MAX: u64 = 1024 * 1024;
/// A prompt in the argv stays below the kernel's per-argument limit (128 KiB).
pub const ARGV_PROMPT_MAX: usize = 128 * 1024 - 1;
/// Answers are cut after this many bytes.
pub const ANSWER_MAX: usize = 256 * 1024;
/// A helper that takes longer is stopped.
pub const TIMEOUT: Duration = Duration::from_secs(240);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Helper {
    pub id: String,
    pub name: String,
    pub command: String,
    /// The one-shot ask: argv[0] is the command; a `{prompt}` element unless `stdin`.
    pub argv: Vec<String>,
    pub stdin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub version: i64,
    /// In preference order; only helpers with a usable `ask`.
    pub helpers: Vec<Helper>,
}

/// An id or a command: a plain name (no path, no spaces, not an option).
pub fn plain(text: &str) -> bool {
    !text.is_empty() && text.len() <= 64 && !text.starts_with(['-', '.']) && text.chars().all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
}

fn clean_name(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(64).collect::<String>().trim().to_string()
}

fn helper(value: &Value) -> Option<Helper> {
    let id = value.get("id")?.as_str().filter(|s| plain(s))?.to_owned();
    let command = value.get("command")?.as_str().filter(|s| plain(s))?.to_owned();
    let name = value.get("name").and_then(Value::as_str).map(clean_name).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone());
    let ask = value.get("ask")?;
    let argv: Vec<String> = ask.get("argv")?.as_array()?.iter().map(|a| a.as_str().map(str::to_owned)).collect::<Option<_>>()?;
    let stdin = ask.get("stdin").and_then(Value::as_bool).unwrap_or(false);
    if argv.is_empty() || argv.len() > 64 || argv[0] != command || argv.iter().any(|a| a.contains('\0')) {
        return None;
    }
    if !stdin && !argv[1..].iter().any(|a| a.contains(PROMPT)) {
        return None;
    }
    Some(Helper { id, name, command, argv, stdin })
}

/// A catalog; None when it isn't one. Entries without an `ask`, or with broken fields, are left
/// out; a repeated id counts once (the first).
pub fn parse(bytes: &[u8]) -> Option<Catalog> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let version = value.get("version")?.as_i64()?;
    let mut helpers: Vec<Helper> = vec![];
    for found in value.get("helpers")?.as_array()?.iter().filter_map(helper) {
        if !helpers.iter().any(|h| h.id == found.id) {
            helpers.push(found);
        }
    }
    Some(Catalog { version, helpers })
}

/// Maprisma's catalog when it is at least as new as the shipped one.
pub fn pick(system: Option<Catalog>, shipped: Catalog) -> Catalog {
    match system {
        Some(system) if system.version >= shipped.version => system,
        _ => shipped,
    }
}

fn read_catalog(path: &Path) -> Option<Catalog> {
    use std::io::Read;
    let mut bytes = vec![];
    std::fs::File::open(path).ok()?.take(CATALOG_MAX).read_to_end(&mut bytes).ok()?;
    parse(&bytes)
}

pub fn catalog() -> Catalog {
    let shipped = parse(SHIPPED.as_bytes()).expect("data/helpers.json");
    let system = if cfg!(test) { None } else { read_catalog(Path::new(SYSTEM_CATALOG)) };
    pick(system, shipped)
}

pub fn which_in(program: &str, path: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    if program.is_empty() || program.contains('/') {
        return None;
    }
    path.split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(program))
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// The first line a program prints, killed after `limit`.
fn first_line(argv: &[&str], limit: Duration) -> Option<String> {
    use std::io::Read;
    let mut child = std::process::Command::new(argv.first()?)
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.take(4096).read_to_string(&mut out).ok()?;
    out.lines().next().map(|l| l.trim().to_string()).filter(|l| !l.is_empty())
}

/// A program: on `path`, in `~/.local/bin` or `~/.npm-global/bin`, else where mise has it
/// (`mise which`). Nothing that would install it is ever run.
pub fn find_command(command: &str, path: &str, home: &Path, mise: Option<&Path>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let extra = [home.join(".local/bin"), home.join(".npm-global/bin")];
    let extra: Vec<&str> = extra.iter().filter_map(|p| p.to_str()).collect();
    let search = std::iter::once(path).chain(extra).collect::<Vec<_>>().join(":");
    if let Some(found) = which_in(command, &search) {
        return Some(found);
    }
    let mise = mise?.to_str()?;
    let out = PathBuf::from(first_line(&[mise, "which", command], Duration::from_secs(5))?);
    (out.is_absolute() && std::fs::metadata(&out).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)).then_some(out)
}

/// A helper whose program was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub helper: Helper,
    pub program: PathBuf,
}

pub fn installed_in(catalog: &Catalog, path: &str, home: &Path, mise: Option<&Path>) -> Vec<Installed> {
    catalog.helpers.iter().filter_map(|h| find_command(&h.command, path, home, mise).map(|program| Installed { helper: h.clone(), program })).collect()
}

/// The installed helpers on this session (its PATH, the user's folders, mise).
pub fn installed() -> Vec<Installed> {
    let path = std::env::var("PATH").unwrap_or_default();
    let home = crate::paths::home();
    let mise = find_command("mise", &path, &home, None);
    installed_in(&catalog(), &path, &home, mise.as_deref())
}

/// The helper to ask, with any extra arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    pub installed: Installed,
    /// `pi:<provider>/<model>`: `--provider <provider> --model <model>`.
    pub extra: Vec<String>,
}

/// The shared choice: an installed id; `pi:<provider>/<model>` through pi; else (auto, unknown,
/// not installed) the first installed helper.
pub fn choose(setting: &str, installed: &[Installed]) -> Option<Chosen> {
    if let Some(model) = setting.strip_prefix("pi:") {
        if let (Some(pi), Some((provider, model))) = (installed.iter().find(|i| i.helper.id == "pi"), model.split_once('/')) {
            if !provider.is_empty() && !model.is_empty() && !provider.starts_with('-') && !model.starts_with('-') {
                return Some(Chosen { installed: pi.clone(), extra: vec!["--provider".into(), provider.into(), "--model".into(), model.into()] });
            }
        }
    }
    let found = installed.iter().find(|i| i.helper.id == setting).or_else(|| installed.first())?;
    Some(Chosen { installed: found.clone(), extra: vec![] })
}

/// The ask's argv: the found program first, then the extra arguments, then the catalog's,
/// `{prompt}` replaced inside single elements (NULs dropped). A prompt alone in an element never
/// starts with `-`, so it can't be read as an option. With `stdin` the `{prompt}` elements go.
pub fn ask_argv(chosen: &Chosen, prompt: &str) -> Vec<String> {
    let h = &chosen.installed.helper;
    let clean: String = prompt.chars().filter(|c| *c != '\0').collect();
    let lone = if clean.starts_with('-') { format!(" {clean}") } else { clean.clone() };
    let mut argv = vec![chosen.installed.program.to_string_lossy().into_owned()];
    argv.extend(chosen.extra.iter().cloned());
    for a in &h.argv[1..] {
        if h.stdin && a.contains(PROMPT) {
            continue;
        }
        argv.push(if a == PROMPT { lone.clone() } else { a.replace(PROMPT, &clean) });
    }
    argv
}

/// One earlier question and its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    pub question: String,
    pub answer: String,
    /// The answer read outside text (a weather result): actions in a follow-up need Enter.
    pub tainted: bool,
    /// When the answer came (seconds since the epoch).
    pub at: u64,
}

/// Follow-ups within this many seconds keep the last exchange.
pub const FOLLOW_UP_SECS: u64 = 45;

pub fn follow_up(last: Option<&Exchange>, now: u64) -> Option<&Exchange> {
    last.filter(|e| now >= e.at && now - e.at <= FOLLOW_UP_SECS)
}

/// English name of a language code, for the helper.
pub fn language_name(code: &str) -> &'static str {
    match code {
        "de" => "German",
        "fr" => "French",
        "es" => "Spanish",
        "it" => "Italian",
        "pt" => "Portuguese",
        "nl" => "Dutch",
        "pl" => "Polish",
        _ => "English",
    }
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.to_string() } else { format!("{}…", text.chars().take(max).collect::<String>()) }
}

/// What the helper may propose, and how (docs/overlay.md "Actions"; actions/schema.rs checks it).
pub const ACTIONS_PREAMBLE: &str = "You have no shell and no tools, but the overlay can carry out a few desktop actions for you. \
To propose them, end your answer with ONE fenced code block whose info string is maprisma-actions, holding a JSON array \
(at most 6 objects) of only these objects:\n\
{\"kind\": \"open_app\", \"name\": \"<the app's name as the user said it>\"}\n\
{\"kind\": \"open_url\", \"url\": \"https://...\"}\n\
{\"kind\": \"dnd\", \"on\": true}   (do not disturb on or off)\n\
{\"kind\": \"theme_mode\", \"mode\": \"light\"}   (light or dark)\n\
{\"kind\": \"volume\", \"percent\": 40}   or \"change\": -10 (-100..100) instead of percent; and/or \"mute\": true\n\
{\"kind\": \"media\", \"command\": \"play_pause\"}   (play_pause, next or previous)\n\
{\"kind\": \"workspace\", \"number\": 2}\n\
{\"kind\": \"move_window\", \"workspace\": 2}   (moves the active window)\n\
{\"kind\": \"close_window\"}   (closes the active window; the user confirms)\n\
{\"kind\": \"weather\", \"location\": \"Berlin\"}   (\"\" for the user's own place; the overlay fetches the forecast and shows it as a card)\n\
{\"kind\": \"run_command\", \"argv\": [\"program\", \"arg\"], \"why\": \"<short reason>\"}   (no shell; runs in the home folder only after the user presses Enter; \
only for what the kinds above can't do; sudo, packages, deleting files, autostart and secrets are always refused)\n\
Use no other kinds or fields. Propose actions only when the user asks for something to be done or for the weather; otherwise just answer. \
Keep the text short and say what is about to happen (\"Opening Firefox.\"), never claim a result you cannot know, and never make up weather numbers: \
the card shows them. If no action fits, say how the user can do it.";

/// The question with a short preamble: who the helper is here, the language, what it may do
/// (the actions block, or answers only), and the last exchange when this is a follow-up.
pub fn build_prompt(question: &str, context: Option<&Exchange>, language: &str, desktop: &str, actions: bool) -> String {
    let mut p = format!(
        "You are the voice and typing assistant of a Linux desktop ({desktop}; Maprisma runs on COSMIC, KDE Plasma and Hyprland). \
         The user spoke or typed the request below into a small overlay. Answer briefly and plainly: a few sentences, or a short list \
         or code block when that helps. Answer in the user's language (the language of the request; if unclear, {}). ",
        language_name(language)
    );
    if actions {
        p.push_str(ACTIONS_PREAMBLE);
    } else {
        p.push_str(
            "You cannot act on the desktop: do not run commands, open apps or change files. If the user asks for an action, \
             say in one or two sentences how to do it, without claiming it was done.",
        );
    }
    p.push_str("\n\n");
    if let Some(e) = context {
        p.push_str("The previous exchange (this is a follow-up):\n");
        p.push_str(&format!("User: {}\nAssistant: {}\n\n", cut(&e.question, 2000), cut(&e.answer, 4000)));
    }
    p.push_str("User: ");
    p.push_str(question);
    p
}

/// The helper's stdout as text: no terminal colour codes or carriage returns, trimmed.
pub fn clean_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                // CSI … final byte, or OSC … BEL/ST, or a single escape.
                match chars.peek() {
                    Some('[') => {
                        chars.next();
                        while let Some(n) = chars.next() {
                            if ('@'..='~').contains(&n) {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        chars.next();
                        while let Some(n) = chars.next() {
                            if n == '\u{7}' {
                                break;
                            }
                            if n == '\u{1b}' {
                                chars.next();
                                break;
                            }
                        }
                    }
                    _ => {
                        chars.next();
                    }
                }
            }
            '\r' => {}
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out.trim().to_string()
}

/// Runs the ask. `Err` holds a message for people (translated).
pub async fn run(argv: Vec<String>, stdin_prompt: Option<String>, work: PathBuf, timeout: Duration) -> Result<String, String> {
    run_with(argv, stdin_prompt, work, timeout, |_| {}).await
}

/// [run], in a process group of its own whose id `started` gets (to stop all of it on cancel).
pub async fn run_with(argv: Vec<String>, stdin_prompt: Option<String>, work: PathBuf, timeout: Duration, started: impl FnOnce(i32)) -> Result<String, String> {
    use maprisma_voice::i18n::{tr, trf};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Some(program) = argv.first() else { return Err(tr("No AI helper is installed. Choose one in Maprisma Manager → AI.")) };
    if stdin_prompt.is_none() && argv.iter().any(|a| a.len() > ARGV_PROMPT_MAX) {
        return Err(tr("The question is too long for this helper."));
    }
    let mut child = tokio::process::Command::new(program)
        .args(&argv[1..])
        .current_dir(&work)
        .env("NO_COLOR", "1")
        .stdin(if stdin_prompt.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .process_group(0)
        .spawn()
        .map_err(|e| trf("Could not start the helper: {0}", &[&e.to_string()]))?;
    if let Some(pid) = child.id() {
        started(pid as i32);
    }
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let stdin_pipe = child.stdin.take();
    let feed = async {
        if let (Some(prompt), Some(mut stdin)) = (stdin_prompt, stdin_pipe) {
            // A helper that exits early (broken pipe) is not an error by itself.
            let _ = stdin.write_all(prompt.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }
    };
    let read_out = async {
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = ANSWER_MAX.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
        kept
    };
    let read_err = async {
        let mut tail: Vec<u8> = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    tail.extend_from_slice(&buf[..n]);
                    if tail.len() > 4096 {
                        tail.drain(..tail.len() - 4096);
                    }
                }
            }
        }
        tail
    };
    let all = async {
        let ((), out, err) = tokio::join!(feed, read_out, read_err);
        let status = child.wait().await;
        (out, err, status)
    };
    let (out, err, status) = tokio::time::timeout(timeout, all).await.map_err(|_| tr("The helper took too long."))?;
    let status = status.map_err(|e| e.to_string())?;
    if !status.success() {
        let why = clean_output(&err).lines().rev().find(|l| !l.trim().is_empty()).map(|l| cut(l.trim(), 200)).unwrap_or_default();
        let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
        return Err(if why.is_empty() { trf("The helper stopped ({0}).", &[&code]) } else { trf("The helper stopped ({0}): {1}", &[&code, &why]) });
    }
    let answer = clean_output(&out);
    if answer.is_empty() {
        return Err(tr("The helper gave no answer."));
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        let dir = crate::paths::test_roots().join(format!("helper-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn program(dir: &Path, name: &str, script: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn fake() -> Catalog {
        parse(br#"{"version": 2, "helpers": [
            {"id": "claude", "name": "Claude Code", "command": "claude", "ask": {"argv": ["claude", "-p", "--tools", "", "{prompt}"], "stdin": false}},
            {"id": "noask", "command": "noask", "ask": null, "task": {"argv": ["noask", "{prompt}"]}},
            {"id": "piped", "name": "Piped", "command": "piped", "ask": {"argv": ["piped", "--ask", "--q={prompt}"], "stdin": true}},
            {"id": "pi", "name": "pi", "command": "pi", "ask": {"argv": ["pi", "-p", "--no-tools", "{prompt}"], "stdin": false}},
            {"id": "path", "command": "/bin/sh", "ask": {"argv": ["/bin/sh", "{prompt}"]}},
            {"id": "noprompt", "command": "np", "ask": {"argv": ["np", "-p"]}},
            {"id": "first", "command": "f", "ask": {"argv": ["{prompt}"]}},
            {"id": "other0", "command": "o", "ask": {"argv": ["x", "{prompt}"]}},
            {"id": "nums", "command": "n", "ask": {"argv": ["n", 3, "{prompt}"]}},
            "garbage",
            {"id": "claude", "command": "again", "ask": {"argv": ["again", "{prompt}"]}}
        ]}"#)
        .unwrap()
    }

    #[test]
    fn catalog_takes_only_usable_asks() {
        let c = fake();
        assert_eq!(c.version, 2);
        let ids: Vec<&str> = c.helpers.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["claude", "piped", "pi"]);
        assert!(c.helpers[1].stdin);
        for bad in [&b""[..], b"[]", b"{}", br#"{"version": "1", "helpers": []}"#, br#"{"version": 1}"#] {
            assert_eq!(parse(bad), None);
        }
        // The shipped copy parses, and Maprisma's newer copy wins.
        let shipped = parse(SHIPPED.as_bytes()).unwrap();
        assert_eq!(shipped.helpers[0].id, "claude");
        assert!(!shipped.helpers.iter().any(|h| h.id == "opencode" || h.id == "goose"), "no ask, no entry");
        assert_eq!(catalog(), shipped);
        let newer = Catalog { version: shipped.version + 1, helpers: vec![] };
        assert_eq!(pick(Some(newer.clone()), shipped.clone()), newer);
        assert_eq!(pick(Some(Catalog { version: shipped.version - 1, helpers: vec![] }), shipped.clone()), shipped);
    }

    #[test]
    fn argv_from_catalog_entries() {
        let c = fake();
        let chosen = Chosen { installed: Installed { helper: c.helpers[0].clone(), program: "/opt/bin/claude".into() }, extra: vec![] };
        let odd = "$(rm -rf ~); `x` \"q\" 'y' {prompt}\nnext";
        assert_eq!(ask_argv(&chosen, odd), vec!["/opt/bin/claude", "-p", "--tools", "", odd]);
        assert_eq!(ask_argv(&chosen, "--help")[4], " --help");
        assert_eq!(ask_argv(&chosen, "a\0b")[4], "ab");
        // stdin: the prompt elements go.
        let piped = Chosen { installed: Installed { helper: c.helpers[1].clone(), program: "/p".into() }, extra: vec![] };
        assert_eq!(ask_argv(&piped, "hello"), vec!["/p", "--ask"]);
        // pi with a model: the extra arguments come before the catalog's.
        let pi = Installed { helper: c.helpers[2].clone(), program: "/bin/pi".into() };
        let chosen = choose("pi:ollama/qwen3:8b", std::slice::from_ref(&pi)).unwrap();
        assert_eq!(ask_argv(&chosen, "q"), vec!["/bin/pi", "--provider", "ollama", "--model", "qwen3:8b", "-p", "--no-tools", "q"]);
    }

    #[test]
    fn shared_helper_resolution() {
        let c = fake();
        let dir = scratch("resolve");
        let (bin, home) = (dir.join("bin"), dir.join("home"));
        program(&bin, "piped", "#!/bin/sh\n");
        std::fs::write(bin.join("claude"), "").unwrap(); // not executable: not installed
        let found = installed_in(&c, bin.to_str().unwrap(), &home, None);
        assert_eq!(found.iter().map(|i| i.helper.id.as_str()).collect::<Vec<_>>(), ["piped"]);
        // ~/.local/bin counts; mise answers for the rest.
        program(&home.join(".local/bin"), "claude", "#!/bin/sh\n");
        let pi = program(&dir.join("mise-installs"), "pi", "#!/bin/sh\n");
        let mise = program(&dir, "mise", &format!("#!/bin/sh\n[ \"$1\" = which ] && [ \"$2\" = pi ] && echo {} && exit 0\nexit 1\n", pi.display()));
        let found = installed_in(&c, &format!("{}:/nonexistent", bin.display()), &home, Some(&mise));
        assert_eq!(found.iter().map(|i| i.helper.id.as_str()).collect::<Vec<_>>(), ["claude", "piped", "pi"]);
        let id = |s: &str| choose(s, &found).map(|c| (c.installed.helper.id, c.extra.len()));
        assert_eq!(id("auto"), Some(("claude".into(), 0)));
        assert_eq!(id("piped"), Some(("piped".into(), 0)));
        assert_eq!(id("hermes"), Some(("claude".into(), 0)), "not installed: auto");
        assert_eq!(id("pi:ollama/qwen"), Some(("pi".into(), 4)));
        assert_eq!(id("pi:broken"), Some(("claude".into(), 0)));
        assert_eq!(id("pi:-x/y"), Some(("claude".into(), 0)));
        assert_eq!(choose("auto", &[]), None);
        // The shared file decides (crate::shared_ai), read each time.
        let shared = crate::shared_ai::file();
        std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
        std::fs::write(&shared, r#"{"helper": "piped"}"#).unwrap();
        assert_eq!(choose(&crate::shared_ai::choice(), &found).unwrap().installed.helper.id, "piped");
        std::fs::remove_file(&shared).unwrap();
        assert_eq!(choose(&crate::shared_ai::choice(), &found).unwrap().installed.helper.id, "claude");
    }

    #[test]
    fn prompt_and_follow_ups() {
        let p = build_prompt("Wie spät ist es?", None, "de", "KDE", false);
        assert!(p.contains("German") && p.contains("cannot act") && p.contains("KDE") && p.ends_with("User: Wie spät ist es?"));
        let p = build_prompt("open firefox", None, "en", "Hyprland", true);
        assert!(p.contains("maprisma-actions") && p.contains("\"kind\": \"run_command\"") && !p.contains("cannot act"));
        // every kind the preamble names parses
        for line in ACTIONS_PREAMBLE.lines().filter(|l| l.starts_with('{')) {
            let json = &line[..line.rfind('}').unwrap() + 1];
            let v: serde_json::Value = serde_json::from_str(json).unwrap_or_else(|e| panic!("{json}: {e}"));
            crate::actions::schema::parse_one(&v).unwrap_or_else(|e| panic!("{json}: {e}"));
        }
        let last = Exchange { question: "q1".into(), answer: "a1".into(), tainted: false, at: 1000 };
        assert_eq!(follow_up(Some(&last), 1045), Some(&last));
        assert_eq!(follow_up(Some(&last), 1046), None);
        assert_eq!(follow_up(Some(&last), 999), None, "clock went back");
        assert_eq!(follow_up(None, 1000), None);
        let p = build_prompt("and tomorrow?", Some(&last), "en", "Hyprland", true);
        assert!(p.contains("User: q1\nAssistant: a1") && p.ends_with("User: and tomorrow?"));
    }

    #[test]
    fn output_is_cleaned() {
        assert_eq!(clean_output(b"\x1b[1;32mHello\x1b[0m\r\n\x1b]0;title\x07world \x08\n"), "Hello\nworld");
        assert_eq!(clean_output(b"  \n"), "");
        assert_eq!(clean_output(&[0xff, b'a']), "\u{fffd}a");
    }

    #[tokio::test]
    async fn running_a_helper() {
        let dir = scratch("run");
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        // The prompt arrives as one argument, unchanged; the answer is stdout.
        let echo = program(&dir, "echo-helper", "#!/bin/sh\nprintf 'cwd=%s\\n' \"$(pwd)\"\nprintf '%s' \"$2\"\n");
        let answer = run(vec![echo.display().to_string(), "-p".into(), "a; $(x) 'b'".into()], None, work.clone(), Duration::from_secs(5)).await.unwrap();
        assert_eq!(answer, format!("cwd={}\na; $(x) 'b'", work.display()));
        // stdin
        let cat = program(&dir, "cat-helper", "#!/bin/sh\ncat\n");
        assert_eq!(run(vec![cat.display().to_string()], Some("from stdin".into()), work.clone(), Duration::from_secs(5)).await.unwrap(), "from stdin");
        // failures: exit code with the last stderr line, no answer, timeout, missing program
        let fail = program(&dir, "fail-helper", "#!/bin/sh\necho 'not signed in' >&2\nexit 3\n");
        let e = run(vec![fail.display().to_string()], None, work.clone(), Duration::from_secs(5)).await.unwrap_err();
        assert!(e.contains('3') && e.contains("not signed in"), "{e}");
        let quiet = program(&dir, "quiet-helper", "#!/bin/sh\nexit 0\n");
        assert!(run(vec![quiet.display().to_string()], None, work.clone(), Duration::from_secs(5)).await.is_err());
        let slow = program(&dir, "slow-helper", "#!/bin/sh\nsleep 5\n");
        let started = std::time::Instant::now();
        assert!(run(vec![slow.display().to_string()], None, work.clone(), Duration::from_millis(300)).await.is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(run(vec!["/nonexistent/helper".into()], None, work.clone(), Duration::from_secs(1)).await.is_err());
        // bounded output
        let big = program(&dir, "big-helper", "#!/bin/sh\nhead -c 600000 /dev/zero | tr '\\0' 'x'\n");
        let answer = run(vec![big.display().to_string()], None, work, Duration::from_secs(10)).await.unwrap();
        assert_eq!(answer.len(), ANSWER_MAX);
    }
}
