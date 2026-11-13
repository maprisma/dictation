//! Markdown-light for answers: paragraphs, headings, bullet and numbered lists, and code
//! (fenced blocks and `inline`). [parse] drops the emphasis markers (plain text); [parse_raw]
//! keeps them for [spans], which the overlay draws as bold, italic and inline code.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading(String),
    Paragraph(String),
    /// A list item: its marker ("•" or "3.") and text.
    Item(String, String),
    Code(String),
}

/// Drops `**`, `__` and single `*`/`_` emphasis around words, keeps `code` as is (without the
/// backticks).
pub fn inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|&x| x == '`') {
                out.extend(&chars[i + 1..i + 1 + end]);
                i += end + 2;
                continue;
            }
        }
        if (c == '*' || c == '_') && i + 1 < chars.len() && chars[i + 1] == c {
            i += 2;
            continue;
        }
        if c == '*' {
            let next_word = chars.get(i + 1).is_some_and(|n| !n.is_whitespace());
            let prev_word = i > 0 && !chars[i - 1].is_whitespace();
            if next_word || prev_word {
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn list_item(line: &str) -> Option<(String, String)> {
    let t = line.trim_start();
    for m in ["- ", "* ", "+ ", "• "] {
        if let Some(rest) = t.strip_prefix(m) {
            return Some(("•".into(), rest.trim().to_string()));
        }
    }
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() && digits.len() <= 3 {
        let rest = &t[digits.len()..];
        if let Some(rest) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((format!("{digits}."), rest.trim().to_string()));
        }
    }
    None
}

/// The blocks as plain text (emphasis markers and backticks dropped).
pub fn parse(text: &str) -> Vec<Block> {
    parse_with(text, inline)
}

/// The blocks with their inline markup kept (for [spans]).
pub fn parse_raw(text: &str) -> Vec<Block> {
    parse_with(text, str::to_string)
}

/// A run of text in one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
}

/// A block's inline markup as styled runs: `**bold**`/`__bold__`, `*italic*` (a `*` next to a
/// word, so "2 * 3" stays) and `` `code` ``. The runs' text together is what [inline] gives.
pub fn spans(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Span> = vec![];
    let (mut bold, mut italic) = (false, false);
    let push = |out: &mut Vec<Span>, t: &str, bold: bool, italic: bool, code: bool| {
        if t.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.bold == bold && last.italic == italic && last.code == code => last.text.push_str(t),
            _ => out.push(Span { text: t.to_string(), bold, italic, code }),
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|&x| x == '`') {
                let code: String = chars[i + 1..i + 1 + end].iter().collect();
                push(&mut out, &code, bold, italic, true);
                i += end + 2;
                continue;
            }
        }
        if (c == '*' || c == '_') && i + 1 < chars.len() && chars[i + 1] == c {
            bold = !bold;
            i += 2;
            continue;
        }
        if c == '*' {
            let next_word = chars.get(i + 1).is_some_and(|n| !n.is_whitespace());
            let prev_word = i > 0 && !chars[i - 1].is_whitespace();
            if next_word || prev_word {
                italic = !italic;
                i += 1;
                continue;
            }
        }
        push(&mut out, &c.to_string(), bold, italic, false);
        i += 1;
    }
    out
}

fn parse_with(text: &str, inline: fn(&str) -> String) -> Vec<Block> {
    let mut blocks = vec![];
    let mut paragraph: Vec<String> = vec![];
    let mut code: Option<Vec<String>> = None;
    let flush = |paragraph: &mut Vec<String>, blocks: &mut Vec<Block>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(inline(&paragraph.join(" "))));
            paragraph.clear();
        }
    };
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            match code.take() {
                Some(lines) => blocks.push(Block::Code(lines.join("\n"))),
                None => {
                    flush(&mut paragraph, &mut blocks);
                    code = Some(vec![]);
                }
            }
            continue;
        }
        if let Some(lines) = code.as_mut() {
            lines.push(line.to_string());
            continue;
        }
        let t = line.trim();
        if t.is_empty() {
            flush(&mut paragraph, &mut blocks);
        } else if let Some(h) = t.strip_prefix('#') {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Heading(inline(h.trim_start_matches('#').trim())));
        } else if let Some((marker, item)) = list_item(line) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Item(marker, inline(&item)));
        } else if matches!(blocks.last(), Some(Block::Item(..))) && paragraph.is_empty() && line.starts_with([' ', '\t']) {
            // A list item's continuation line.
            if let Some(Block::Item(_, text)) = blocks.last_mut() {
                text.push(' ');
                text.push_str(&inline(t));
            }
        } else {
            paragraph.push(t.to_string());
        }
    }
    // An unclosed fence still shows its code.
    if let Some(lines) = code {
        blocks.push(Block::Code(lines.join("\n")));
    }
    flush(&mut paragraph, &mut blocks);
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks() {
        let text = "## Steps\nOpen the **settings**\nand go on.\n\n- first `item`\n  continued\n* second\n2) numbered\n\n```sh\nls -la\n  echo *x*\n```\nEnd *here*.";
        assert_eq!(
            parse(text),
            vec![
                Block::Heading("Steps".into()),
                Block::Paragraph("Open the settings and go on.".into()),
                Block::Item("•".into(), "first item continued".into()),
                Block::Item("•".into(), "second".into()),
                Block::Item("2.".into(), "numbered".into()),
                Block::Code("ls -la\n  echo *x*".into()),
                Block::Paragraph("End here.".into()),
            ]
        );
        assert_eq!(parse("```\nopen"), vec![Block::Code("open".into())]);
        assert_eq!(parse(""), vec![]);
        assert_eq!(inline("2 * 3 = 6, a_b"), "2 * 3 = 6, a_b");
    }

    #[test]
    fn styled_runs() {
        let s = |t: &str, bold: bool, italic: bool, code: bool| Span { text: t.into(), bold, italic, code };
        assert_eq!(
            spans("Ja, **Yamaha** ist *gut*: `FG800` und mehr"),
            vec![s("Ja, ", false, false, false), s("Yamaha", true, false, false), s(" ist ", false, false, false), s("gut", false, true, false), s(": ", false, false, false), s("FG800", false, false, true), s(" und mehr", false, false, false)]
        );
        assert_eq!(spans("2 * 3 = 6, a_b"), vec![s("2 * 3 = 6, a_b", false, false, false)]);
        assert_eq!(spans("**bold `code`**"), vec![s("bold ", true, false, false), s("code", true, false, true)]);
        assert_eq!(spans(""), vec![]);
        // The runs are the plain text, and parse_raw keeps what parse drops.
        for t in ["Open the **settings** and *go* on", "x `a*b` y", "**unclosed", "a__b__c"] {
            assert_eq!(spans(t).iter().map(|s| s.text.as_str()).collect::<String>(), inline(t), "{t}");
        }
        assert_eq!(parse_raw("## **H**\n- *i*"), vec![Block::Heading("**H**".into()), Block::Item("•".into(), "*i*".into())]);
    }
}
