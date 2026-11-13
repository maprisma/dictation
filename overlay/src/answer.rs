//! How an answer shows (docs/overlay.md "Commands and questions"): a command's answer is its
//! chips, and the overlay closes by itself once they all ran; a question's answer opens the
//! wider reading view. Pure decisions, kept free of the UI so they are tested on their own.

use crate::actions::{Item, State};
use crate::actions::policy::Level;
use crate::markdown::Block;

/// A command's answer has at most this much text besides its actions block.
pub const COMMAND_TEXT_MAX: usize = 40;
/// An answer with actions and at least this much text reads like a question's answer.
pub const LONG_TEXT: usize = 280;
/// The chips stay this long before the overlay closes …
pub const CLOSE_MS: u64 = 1200;
/// … or this long when one of them can be undone.
pub const UNDO_CLOSE_MS: u64 = 3000;

/// The reading view's width (logical px; at most 80 % of the output's width) …
pub const READING_WIDTH: f32 = 720.0;
/// … and its height: at most 70 % of the output's height, never less than the compact maximum.
pub const READING_HEIGHT_SHARE: f32 = 0.70;
/// When the output's size is not known.
pub const READING_FALLBACK_HEIGHT: f32 = 560.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// The pill as it always was (thinking, errors, actions with a few sentences).
    #[default]
    Compact,
    /// A command: chips only; may close by itself.
    Command,
    /// A question's answer: wider and taller, larger type.
    Reading,
}

/// `text`: the answer without its actions block; `actions`: how many actions it proposed
/// (unreadable ones too).
pub fn classify(text: &str, actions: usize) -> View {
    let length = text.trim().chars().count();
    if actions > 0 && length <= COMMAND_TEXT_MAX {
        View::Command
    } else if actions == 0 || length >= LONG_TEXT {
        View::Reading
    } else {
        View::Compact
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoClose {
    /// Actions still wait or run: decide later.
    Pending,
    /// Something to read or decide: stay open.
    Stay,
    /// Close after this many milliseconds (`undo`: an action can be undone meanwhile).
    After { millis: u64, undo: bool },
}

/// Only a command whose actions all ran at once (level 0) and worked closes by itself: an Enter
/// card, a blocked or unreadable action, an error, a choice of apps, a command's output, an undo
/// and a weather card (that is an answer) keep it open.
pub fn auto_close(view: View, items: &[Item]) -> AutoClose {
    if view != View::Command || items.is_empty() {
        return AutoClose::Stay;
    }
    let mut undo = false;
    for item in items {
        match &item.state {
            State::Waiting | State::Running => return AutoClose::Pending,
            State::Done { undo: u, .. } if item.level == Level::Run => undo |= u.is_some(),
            _ => return AutoClose::Stay,
        }
    }
    AutoClose::After { millis: if undo { UNDO_CLOSE_MS } else { CLOSE_MS }, undo }
}

/// The reading view's width and largest height for an output of this logical size.
pub fn reading_size(output: Option<(f32, f32)>, compact: (f32, f32)) -> (f32, f32) {
    match output {
        Some((w, h)) if w > 0.0 && h > 0.0 => (READING_WIDTH.min(w * 0.8).max(compact.0.min(w)), (h * READING_HEIGHT_SHARE).max(compact.1.min(h))),
        _ => (READING_WIDTH, READING_FALLBACK_HEIGHT),
    }
}

/// The output the overlay opens on: the one asked for by name, else the only one, else the
/// focused one (Hyprland tells), else the smallest (the view fits everywhere then).
pub fn pick_output(outputs: &[(String, (f32, f32))], wanted: Option<&str>, focused: Option<&str>) -> Option<(f32, f32)> {
    let named = |name: Option<&str>| name.and_then(|n| outputs.iter().find(|(o, _)| o == n)).map(|(_, s)| *s);
    if let Some(size) = named(wanted) {
        return Some(size);
    }
    if outputs.len() == 1 {
        return Some(outputs[0].1);
    }
    named(focused).or_else(|| outputs.iter().map(|(_, s)| *s).min_by(|a, b| (a.0 * a.1).total_cmp(&(b.0 * b.1))))
}

/// The space above a block in the reading view (the spacing steps xs 4, sm 8, lg 16): a
/// section's heading stands apart, its text follows it closely, list items sit close together.
pub fn gap_before(previous: Option<&Block>, block: &Block) -> f32 {
    match (previous, block) {
        (None, _) => 0.0,
        (Some(_), Block::Heading(_)) => 16.0,
        (Some(Block::Heading(_)), _) => 4.0,
        (Some(Block::Item(..)), Block::Item(..)) => 4.0,
        _ => 8.0,
    }
}

/// The direct answer: the first block when it is a paragraph (shown a little larger).
pub fn is_lead(blocks: &[Block], i: usize) -> bool {
    i == 0 && matches!(blocks.first(), Some(Block::Paragraph(_)))
}

/// Whole seconds left of an auto-close, for the countdown (rounded up).
pub fn seconds_left(total_ms: u64, elapsed_ms: u64) -> u64 {
    total_ms.saturating_sub(elapsed_ms).div_ceil(1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::exec::{Outcome, Undo};
    use crate::actions::{Turn, schema};
    use crate::markdown;

    fn turn(answer: &str) -> (View, Turn) {
        let split = schema::split(answer);
        let t = Turn::new("q", &split, false, false, 1);
        (classify(&split.text, t.items.len()), t)
    }

    #[test]
    fn commands_and_questions() {
        let block = "```maprisma-actions\n[{\"kind\": \"open_url\", \"url\": \"https://www.youtube.com\"}]\n```";
        assert_eq!(turn(block).0, View::Command, "only the block");
        assert_eq!(turn(&format!("Öffne YouTube.\n{block}")).0, View::Command, "a short confirmation");
        assert_eq!(turn(&format!("I'll open YouTube for you now, so you can watch videos there.\n{block}")).0, View::Compact, "a few sentences with actions");
        assert_eq!(turn(&format!("{}\n{block}", "Long text. ".repeat(40))).0, View::Reading, "long text with actions");
        assert_eq!(turn("Ja, Yamaha-Gitarren sind gut.\n\n## Vorteile\n- solide").0, View::Reading);
        assert_eq!(turn("Noon.").0, View::Reading, "no actions: an answer");
        // An unreadable block counts as an action (its chip shows).
        assert_eq!(turn("```maprisma-actions\n[{\"kind\": \"bogus\"}]\n```").0, View::Command);
    }

    #[test]
    fn closing_by_itself() {
        let done = |t: &mut Turn, i: usize, undo: Option<Undo>| {
            assert!(matches!(t.advance(), crate::actions::Next::Run(n) if n == i));
            t.finished(i, Outcome::Done { text: "Opened YouTube".into(), undo }, 2);
        };
        // all level 0, all worked: close after 1.2 s
        let (view, mut t) = turn("```maprisma-actions\n[{\"kind\": \"open_url\", \"url\": \"https://youtube.com\"}, {\"kind\": \"workspace\", \"number\": 2}]\n```");
        assert_eq!(auto_close(view, &t.items), AutoClose::Pending, "nothing ran yet");
        done(&mut t, 0, None);
        assert_eq!(auto_close(view, &t.items), AutoClose::Pending, "the second still waits");
        done(&mut t, 1, None);
        assert_eq!(auto_close(view, &t.items), AutoClose::After { millis: CLOSE_MS, undo: false });
        // one can be undone: 3 s
        let (view, mut t) = turn("Leiser.\n```maprisma-actions\n[{\"kind\": \"volume\", \"change\": -10}]\n```");
        done(&mut t, 0, Some(Undo::Volume(0.5, false)));
        assert_eq!(auto_close(view, &t.items), AutoClose::After { millis: UNDO_CLOSE_MS, undo: true });
        // … and once undone it stays
        t.start_undo(0);
        assert_eq!(auto_close(view, &t.items), AutoClose::Stay);
        // an Enter card, also after it ran
        let (view, mut t) = turn("```maprisma-actions\n[{\"kind\": \"close_window\"}]\n```");
        assert_eq!(t.advance(), crate::actions::Next::Confirm(0));
        assert_eq!(auto_close(view, &t.items), AutoClose::Stay);
        t.confirm();
        t.finished(0, Outcome::Done { text: "Closed".into(), undo: None }, 2);
        assert_eq!(auto_close(view, &t.items), AutoClose::Stay, "it needed Enter");
        // blocked, unreadable
        assert_eq!(auto_close(turn("```maprisma-actions\n[{\"kind\": \"run_command\", \"argv\": [\"sudo\", \"x\"], \"why\": \"\"}]\n```").0, &turn("```maprisma-actions\n[{\"kind\": \"run_command\", \"argv\": [\"sudo\", \"x\"], \"why\": \"\"}]\n```").1.items), AutoClose::Stay);
        let (view, t) = turn("```maprisma-actions\n[{\"kind\": \"bogus\"}]\n```");
        assert_eq!(auto_close(view, &t.items), AutoClose::Stay);
        // an error
        let (view, mut t) = turn("```maprisma-actions\n[{\"kind\": \"open_app\", \"name\": \"nothing\"}]\n```");
        t.advance();
        t.finished(0, Outcome::Failed("No app called nothing".into()), 2);
        assert_eq!(auto_close(view, &t.items), AutoClose::Stay);
        // weather is an answer
        let (view, mut t) = turn("```maprisma-actions\n[{\"kind\": \"weather\", \"location\": \"Berlin\"}]\n```");
        t.advance();
        let place = crate::actions::weather::Place { name: "Berlin".into(), region: String::new(), latitude: 0.0, longitude: 0.0 };
        let w = crate::actions::weather::Weather { place, temperature: 8.0, feels_like: None, code: 3, day: true, wind_kmh: None, high: None, low: None, rain_chance: None, hours: vec![] };
        t.finished(0, Outcome::Weather(Box::new(w)), 2);
        assert_eq!(auto_close(view, &t.items), AutoClose::Stay);
        // never for a question's or a compact answer
        assert_eq!(auto_close(View::Reading, &[]), AutoClose::Stay);
        assert_eq!(auto_close(View::Compact, &t.items), AutoClose::Stay);
        assert_eq!(seconds_left(3000, 0), 3);
        assert_eq!(seconds_left(3000, 2100), 1);
        assert_eq!(seconds_left(3000, 3500), 0);
    }

    #[test]
    fn reading_view_size() {
        let compact = (480.0, 360.0);
        assert_eq!(reading_size(Some((1920.0, 1080.0)), compact), (720.0, 756.0));
        assert_eq!(reading_size(Some((800.0, 600.0)), compact), (640.0, 420.0), "80 % and 70 %");
        assert_eq!(reading_size(Some((500.0, 400.0)), compact), (480.0, 360.0), "never below the compact size …");
        assert_eq!(reading_size(Some((400.0, 300.0)), compact), (400.0, 300.0), "… unless the output is smaller");
        assert_eq!(reading_size(None, compact), (READING_WIDTH, READING_FALLBACK_HEIGHT));
        let outputs = vec![("DP-1".to_string(), (2560.0, 1440.0)), ("HDMI-A-1".to_string(), (1920.0, 1080.0))];
        assert_eq!(pick_output(&outputs, Some("DP-1"), Some("HDMI-A-1")), Some((2560.0, 1440.0)));
        assert_eq!(pick_output(&outputs, None, Some("DP-1")), Some((2560.0, 1440.0)));
        assert_eq!(pick_output(&outputs, Some("gone"), None), Some((1920.0, 1080.0)), "the smallest");
        assert_eq!(pick_output(&outputs[1..], None, None), Some((1920.0, 1080.0)));
        assert_eq!(pick_output(&[], None, None), None);
    }

    #[test]
    fn reading_layout() {
        let blocks = markdown::parse_raw("Ja, sie sind gut.\n\n## Vorteile\n- **solide**\n- günstig\n\nFazit.\n\n## Empfehlung\nDie FG800.");
        let gaps: Vec<f32> = (0..blocks.len()).map(|i| gap_before(i.checked_sub(1).map(|p| &blocks[p]), &blocks[i])).collect();
        assert_eq!(gaps, [0.0, 16.0, 4.0, 4.0, 8.0, 16.0, 4.0]);
        assert!(is_lead(&blocks, 0) && !is_lead(&blocks, 1));
        assert!(!is_lead(&markdown::parse_raw("## Title\nText"), 0), "a heading first: no lead");
    }
}
