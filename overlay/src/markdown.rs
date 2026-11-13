//! Markdown-light for answers: paragraphs, headings, bullet and numbered lists, and code
//! (fenced blocks and `inline`). Emphasis markers are dropped; everything else stays text.

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

pub fn parse(text: &str) -> Vec<Block> {
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
}
