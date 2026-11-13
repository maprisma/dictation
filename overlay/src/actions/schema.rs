//! The helper's proposed actions: ONE fenced block ```maprisma-actions holding a JSON array of
//! objects from a fixed schema (docs/overlay.md "Actions"). Parsing is strict and bounded: an
//! unknown kind, an unknown or missing field, a wrong type or a value out of range rejects that
//! object; a second block, a block that is too big or not an array rejects them all.

use serde_json::{Map, Value};

pub const FENCE: &str = "maprisma-actions";
/// The block's JSON is at most this many bytes.
pub const BLOCK_MAX: usize = 8 * 1024;
/// At most this many actions per answer.
pub const ACTIONS_MAX: usize = 6;
pub const NAME_MAX: usize = 100;
pub const URL_MAX: usize = 2000;
pub const ARGV_MAX: usize = 32;
pub const ARG_MAX: usize = 1024;
pub const ARGV_BYTES_MAX: usize = 4096;
pub const WHY_MAX: usize = 300;
pub const WORKSPACE_MAX: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    PlayPause,
    Next,
    Previous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    OpenApp { name: String },
    OpenUrl { url: String },
    Dnd { on: bool },
    ThemeMode { mode: ThemeMode },
    /// At least one of them; `percent` and `change` not both.
    Volume { percent: Option<u32>, mute: Option<bool>, change: Option<i32> },
    Media { command: Media },
    Workspace { number: u32 },
    MoveWindow { workspace: u32 },
    Weather { location: String },
    CloseWindow,
    RunCommand { argv: Vec<String>, why: String },
}

impl Action {
    pub fn kind(&self) -> &'static str {
        match self {
            Action::OpenApp { .. } => "open_app",
            Action::OpenUrl { .. } => "open_url",
            Action::Dnd { .. } => "dnd",
            Action::ThemeMode { .. } => "theme_mode",
            Action::Volume { .. } => "volume",
            Action::Media { .. } => "media",
            Action::Workspace { .. } => "workspace",
            Action::MoveWindow { .. } => "move_window",
            Action::Weather { .. } => "weather",
            Action::CloseWindow => "close_window",
            Action::RunCommand { .. } => "run_command",
        }
    }

    /// The action as JSON again (for the log).
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("kind".into(), self.kind().into());
        match self {
            Action::OpenApp { name } => {
                m.insert("name".into(), name.as_str().into());
            }
            Action::OpenUrl { url } => {
                m.insert("url".into(), url.as_str().into());
            }
            Action::Dnd { on } => {
                m.insert("on".into(), (*on).into());
            }
            Action::ThemeMode { mode } => {
                m.insert("mode".into(), (if *mode == ThemeMode::Light { "light" } else { "dark" }).into());
            }
            Action::Volume { percent, mute, change } => {
                if let Some(p) = percent {
                    m.insert("percent".into(), (*p).into());
                }
                if let Some(x) = mute {
                    m.insert("mute".into(), (*x).into());
                }
                if let Some(c) = change {
                    m.insert("change".into(), (*c).into());
                }
            }
            Action::Media { command } => {
                m.insert("command".into(), media_name(*command).into());
            }
            Action::Workspace { number } => {
                m.insert("number".into(), (*number).into());
            }
            Action::MoveWindow { workspace } => {
                m.insert("workspace".into(), (*workspace).into());
            }
            Action::Weather { location } => {
                m.insert("location".into(), location.as_str().into());
            }
            Action::CloseWindow => {}
            Action::RunCommand { argv, why } => {
                m.insert("argv".into(), argv.clone().into());
                m.insert("why".into(), why.as_str().into());
            }
        }
        Value::Object(m)
    }
}

pub fn media_name(m: Media) -> &'static str {
    match m {
        Media::PlayPause => "play_pause",
        Media::Next => "next",
        Media::Previous => "previous",
    }
}

/// An answer split into the text people read and the proposed actions.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Split {
    pub text: String,
    /// One entry per proposed object: the action, or why it was rejected (English, for the log).
    pub actions: Vec<Result<Action, String>>,
    /// The whole block was rejected (two blocks, too big, not a JSON array…).
    pub rejected: Option<String>,
}

/// Finds the block: a line that is exactly ```maprisma-actions, up to the next line that is ```.
pub fn split(answer: &str) -> Split {
    let mut text = String::new();
    let mut blocks: Vec<String> = vec![];
    let mut inside: Option<String> = None;
    let mut unclosed = false;
    for line in answer.split_inclusive('\n') {
        let bare = line.trim();
        match &mut inside {
            Some(body) => {
                if bare == "```" {
                    blocks.push(std::mem::take(body));
                    inside = None;
                } else {
                    body.push_str(line);
                }
            }
            None => {
                if bare.strip_prefix("```").is_some_and(|tag| tag.trim() == FENCE) {
                    inside = Some(String::new());
                } else {
                    text.push_str(line);
                }
            }
        }
    }
    if let Some(body) = inside {
        // An unclosed block (the answer was cut): still not text.
        blocks.push(body);
        unclosed = true;
    }
    let mut out = Split { text: text.trim().to_string(), ..Default::default() };
    match blocks.as_slice() {
        [] => {}
        [_, _, ..] => out.rejected = Some("more than one actions block".into()),
        [_] if unclosed => out.rejected = Some("the actions block is not closed".into()),
        [body] if body.len() > BLOCK_MAX => out.rejected = Some(format!("the actions block is over {BLOCK_MAX} bytes")),
        [body] => match serde_json::from_str::<Value>(body) {
            Ok(Value::Array(items)) if items.len() > ACTIONS_MAX => out.rejected = Some(format!("more than {ACTIONS_MAX} actions")),
            Ok(Value::Array(items)) => out.actions = items.iter().map(parse_one).collect(),
            Ok(_) => out.rejected = Some("the actions block is not a JSON array".into()),
            Err(e) => out.rejected = Some(format!("the actions block is not JSON ({e})")),
        },
    }
    out
}

fn text_field(m: &Map<String, Value>, key: &str, max: usize, allow_empty: bool) -> Result<String, String> {
    let s = m.get(key).and_then(Value::as_str).ok_or_else(|| format!("`{key}` must be a string"))?;
    let s = s.trim();
    if s.chars().count() > max {
        return Err(format!("`{key}` is too long"));
    }
    if !allow_empty && s.is_empty() {
        return Err(format!("`{key}` is empty"));
    }
    if s.chars().any(|c| c.is_control()) {
        return Err(format!("`{key}` has control characters"));
    }
    Ok(s.to_string())
}

fn int_field(m: &Map<String, Value>, key: &str) -> Result<i64, String> {
    m.get(key).and_then(Value::as_i64).ok_or_else(|| format!("`{key}` must be a whole number"))
}

fn bool_field(m: &Map<String, Value>, key: &str) -> Result<bool, String> {
    m.get(key).and_then(Value::as_bool).ok_or_else(|| format!("`{key}` must be true or false"))
}

/// Only these fields (besides `kind`); `required` must be there.
fn fields(m: &Map<String, Value>, allowed: &[&str], required: &[&str]) -> Result<(), String> {
    for k in m.keys() {
        if k != "kind" && !allowed.contains(&k.as_str()) {
            return Err(format!("unknown field `{k}`"));
        }
    }
    for k in required {
        if !m.contains_key(*k) {
            return Err(format!("missing field `{k}`"));
        }
    }
    Ok(())
}

/// An http(s) URL with a host, no spaces or control characters.
pub fn web_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let rest = if let Some(r) = lower.strip_prefix("https://") {
        r
    } else if let Some(r) = lower.strip_prefix("http://") {
        r
    } else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or("");
    !host.is_empty() && !host.starts_with(':') && url.len() <= URL_MAX && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn workspace_number(m: &Map<String, Value>, key: &str) -> Result<u32, String> {
    let n = int_field(m, key)?;
    if !(1..=WORKSPACE_MAX as i64).contains(&n) {
        return Err(format!("`{key}` must be 1–{WORKSPACE_MAX}"));
    }
    Ok(n as u32)
}

pub fn parse_one(value: &Value) -> Result<Action, String> {
    let Value::Object(m) = value else { return Err("an action must be a JSON object".into()) };
    let kind = m.get("kind").and_then(Value::as_str).ok_or("missing `kind`")?;
    match kind {
        "open_app" => {
            fields(m, &["name"], &["name"])?;
            Ok(Action::OpenApp { name: text_field(m, "name", NAME_MAX, false)? })
        }
        "open_url" => {
            fields(m, &["url"], &["url"])?;
            let url = text_field(m, "url", URL_MAX, false)?;
            if !web_url(&url) {
                return Err("`url` must be an http or https address".into());
            }
            Ok(Action::OpenUrl { url })
        }
        "dnd" => {
            fields(m, &["on"], &["on"])?;
            Ok(Action::Dnd { on: bool_field(m, "on")? })
        }
        "theme_mode" => {
            fields(m, &["mode"], &["mode"])?;
            match m.get("mode").and_then(Value::as_str) {
                Some("light") => Ok(Action::ThemeMode { mode: ThemeMode::Light }),
                Some("dark") => Ok(Action::ThemeMode { mode: ThemeMode::Dark }),
                _ => Err("`mode` must be light or dark".into()),
            }
        }
        "volume" => {
            fields(m, &["percent", "mute", "change"], &[])?;
            let percent = match m.get("percent") {
                None => None,
                Some(_) => {
                    let p = int_field(m, "percent")?;
                    if !(0..=100).contains(&p) {
                        return Err("`percent` must be 0–100".into());
                    }
                    Some(p as u32)
                }
            };
            let change = match m.get("change") {
                None => None,
                Some(_) => {
                    let c = int_field(m, "change")?;
                    if !(-100..=100).contains(&c) || c == 0 {
                        return Err("`change` must be −100…100 and not 0".into());
                    }
                    Some(c as i32)
                }
            };
            let mute = match m.get("mute") {
                None => None,
                Some(_) => Some(bool_field(m, "mute")?),
            };
            if percent.is_some() && change.is_some() {
                return Err("`percent` and `change` together".into());
            }
            if percent.is_none() && change.is_none() && mute.is_none() {
                return Err("volume needs `percent`, `change` or `mute`".into());
            }
            Ok(Action::Volume { percent, mute, change })
        }
        "media" => {
            fields(m, &["command"], &["command"])?;
            let command = match m.get("command").and_then(Value::as_str) {
                Some("play_pause") => Media::PlayPause,
                Some("next") => Media::Next,
                Some("previous") => Media::Previous,
                _ => return Err("`command` must be play_pause, next or previous".into()),
            };
            Ok(Action::Media { command })
        }
        "workspace" => {
            fields(m, &["number"], &["number"])?;
            Ok(Action::Workspace { number: workspace_number(m, "number")? })
        }
        "move_window" => {
            fields(m, &["workspace"], &["workspace"])?;
            Ok(Action::MoveWindow { workspace: workspace_number(m, "workspace")? })
        }
        "weather" => {
            fields(m, &["location"], &[])?;
            let location = if m.contains_key("location") { text_field(m, "location", NAME_MAX, true)? } else { String::new() };
            Ok(Action::Weather { location })
        }
        "close_window" => {
            fields(m, &[], &[])?;
            Ok(Action::CloseWindow)
        }
        "run_command" => {
            fields(m, &["argv", "why"], &["argv", "why"])?;
            let Some(Value::Array(items)) = m.get("argv") else { return Err("`argv` must be an array of strings".into()) };
            if items.is_empty() || items.len() > ARGV_MAX {
                return Err(format!("`argv` must have 1–{ARGV_MAX} elements"));
            }
            let mut argv = vec![];
            for item in items {
                let s = item.as_str().ok_or("`argv` must be an array of strings")?;
                if s.len() > ARG_MAX || s.contains('\0') {
                    return Err("an `argv` element is too long or has a NUL".into());
                }
                argv.push(s.to_string());
            }
            if argv.iter().map(String::len).sum::<usize>() > ARGV_BYTES_MAX {
                return Err("`argv` is too long".into());
            }
            if argv[0].trim().is_empty() || argv[0] != argv[0].trim() {
                return Err("`argv[0]` must be a program".into());
            }
            Ok(Action::RunCommand { argv, why: text_field(m, "why", WHY_MAX, true)? })
        }
        other => Err(format!("unknown kind `{}`", other.chars().take(40).collect::<String>())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one(v: Value) -> Result<Action, String> {
        parse_one(&v)
    }

    #[test]
    fn valid_actions() {
        assert_eq!(one(json!({"kind": "open_app", "name": " Firefox "})), Ok(Action::OpenApp { name: "Firefox".into() }));
        assert_eq!(one(json!({"kind": "open_url", "url": "https://example.org/a?b"})), Ok(Action::OpenUrl { url: "https://example.org/a?b".into() }));
        assert_eq!(one(json!({"kind": "dnd", "on": true})), Ok(Action::Dnd { on: true }));
        assert_eq!(one(json!({"kind": "theme_mode", "mode": "dark"})), Ok(Action::ThemeMode { mode: ThemeMode::Dark }));
        assert_eq!(one(json!({"kind": "volume", "change": -10})), Ok(Action::Volume { percent: None, mute: None, change: Some(-10) }));
        assert_eq!(one(json!({"kind": "volume", "percent": 40, "mute": false})), Ok(Action::Volume { percent: Some(40), mute: Some(false), change: None }));
        assert_eq!(one(json!({"kind": "media", "command": "next"})), Ok(Action::Media { command: Media::Next }));
        assert_eq!(one(json!({"kind": "workspace", "number": 3})), Ok(Action::Workspace { number: 3 }));
        assert_eq!(one(json!({"kind": "move_window", "workspace": 2})), Ok(Action::MoveWindow { workspace: 2 }));
        assert_eq!(one(json!({"kind": "weather", "location": "Berlin"})), Ok(Action::Weather { location: "Berlin".into() }));
        assert_eq!(one(json!({"kind": "weather"})), Ok(Action::Weather { location: String::new() }));
        assert_eq!(one(json!({"kind": "close_window"})), Ok(Action::CloseWindow));
        let run = one(json!({"kind": "run_command", "argv": ["uname", "-r"], "why": "kernel version"})).unwrap();
        assert_eq!(run, Action::RunCommand { argv: vec!["uname".into(), "-r".into()], why: "kernel version".into() });
        // round trip for the log
        assert_eq!(parse_one(&run.to_json()), Ok(run));
    }

    #[test]
    fn invalid_actions() {
        for bad in [
            json!("open_app"),
            json!({"name": "x"}),
            json!({"kind": "format_disk"}),
            json!({"kind": "open_app", "name": "x", "extra": 1}),
            json!({"kind": "open_app"}),
            json!({"kind": "open_app", "name": ""}),
            json!({"kind": "open_app", "name": 5}),
            json!({"kind": "open_app", "name": "a".repeat(101)}),
            json!({"kind": "open_app", "name": "a\u{1b}[31m"}),
            json!({"kind": "open_url", "url": "file:///etc/passwd"}),
            json!({"kind": "open_url", "url": "javascript:alert(1)"}),
            json!({"kind": "open_url", "url": "https://"}),
            json!({"kind": "open_url", "url": "https://a b"}),
            json!({"kind": "dnd", "on": "yes"}),
            json!({"kind": "theme_mode", "mode": "blue"}),
            json!({"kind": "volume"}),
            json!({"kind": "volume", "percent": 140}),
            json!({"kind": "volume", "percent": 40, "change": 5}),
            json!({"kind": "volume", "change": 0}),
            json!({"kind": "volume", "percent": 4.5}),
            json!({"kind": "media", "command": "stop"}),
            json!({"kind": "workspace", "number": 0}),
            json!({"kind": "workspace", "number": 21}),
            json!({"kind": "move_window", "number": 2}),
            json!({"kind": "close_window", "window": "x"}),
            json!({"kind": "run_command", "argv": [], "why": ""}),
            json!({"kind": "run_command", "argv": "uname -r", "why": ""}),
            json!({"kind": "run_command", "argv": ["uname", 5], "why": ""}),
            json!({"kind": "run_command", "argv": ["uname"]}),
            json!({"kind": "run_command", "argv": [" uname"], "why": ""}),
            json!({"kind": "run_command", "argv": ["a\u{0}b"], "why": ""}),
            json!({"kind": "run_command", "argv": vec!["x"; 33], "why": ""}),
            json!({"kind": "run_command", "argv": ["echo", "a".repeat(1025)], "why": ""}),
        ] {
            assert!(one(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn blocks_in_answers() {
        let s = split("Opening it.\n\n```maprisma-actions\n[{\"kind\": \"open_app\", \"name\": \"Firefox\"}, {\"kind\": \"nope\"}]\n```\n");
        assert_eq!(s.text, "Opening it.");
        assert_eq!(s.actions.len(), 2);
        assert_eq!(s.actions[0], Ok(Action::OpenApp { name: "Firefox".into() }));
        assert!(s.actions[1].is_err());
        assert_eq!(s.rejected, None);
        // no block: all text; other code blocks stay
        let s = split("Run:\n```\nls\n```");
        assert_eq!(s.text, "Run:\n```\nls\n```");
        assert!(s.actions.is_empty() && s.rejected.is_none());
        // two blocks, unclosed, not an array, not JSON, oversized, too many
        let block = "```maprisma-actions\n[]\n```\n";
        assert!(split(&format!("{block}{block}")).rejected.is_some());
        assert!(split("x\n```maprisma-actions\n[{\"kind\": \"close_window\"}]").rejected.is_some());
        assert!(split("```maprisma-actions\n{\"kind\": \"close_window\"}\n```").rejected.is_some());
        assert!(split("```maprisma-actions\n[{kind: close_window}]\n```").rejected.is_some());
        let big = format!("```maprisma-actions\n[{{\"kind\": \"open_app\", \"name\": \"{}\"}}]\n```", "a".repeat(BLOCK_MAX));
        let s = split(&big);
        assert!(s.rejected.unwrap().contains("bytes") && s.actions.is_empty());
        let many = format!("```maprisma-actions\n[{}]\n```", vec![r#"{"kind": "close_window"}"#; 7].join(","));
        assert!(split(&many).rejected.unwrap().contains("more than"));
        // the block alone: no text
        let s = split("```maprisma-actions\n[{\"kind\": \"weather\", \"location\": \"Berlin\"}]\n```");
        assert_eq!(s.text, "");
        assert_eq!(s.actions, vec![Ok(Action::Weather { location: "Berlin".into() })]);
    }
}
