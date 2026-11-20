//! the hover card: what the language server says about the name under the
//! caret. its markdown is read just enough for a card: code blocks in the
//! editor font with a few colours, prose wrapped, rules between parts.

use aurea::AureaResult;
use aurea::render::{Color, Font, Rect};
use aurea::ui::{Align, Palette, Ui};

use super::MONO;
use crate::app::Id;
use crate::syntax::colour_on;
use sylven::HighlightKind;

/// lines a card shows at most, code and prose together.
const MAX_LINES: usize = 30;
/// widest a card gets.
const MAX_WIDTH: f32 = 560.0;
const PAD: f32 = 10.0;

#[derive(Debug, PartialEq)]
pub(crate) enum Block {
    Code(Vec<String>),
    Text(String),
    Heading(String),
    Rule,
}

/// markdown into blocks: fences, `---`, headings, paragraphs. links keep
/// their text, `**` and backticks go.
pub(crate) fn blocks(markdown: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut code: Option<Vec<String>> = None;
    let mut para = String::new();
    let flush = |para: &mut String, out: &mut Vec<Block>| {
        if !para.trim().is_empty() {
            out.push(Block::Text(plain(para.trim())));
        }
        para.clear();
    };
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            match code.take() {
                Some(lines) => out.push(Block::Code(lines)),
                None => {
                    flush(&mut para, &mut out);
                    code = Some(Vec::new());
                }
            }
            continue;
        }
        if let Some(lines) = &mut code {
            lines.push(line.replace('\t', "    "));
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut para, &mut out);
        } else if trimmed.chars().all(|c| matches!(c, '-' | '_' | '*')) && trimmed.len() >= 3 {
            flush(&mut para, &mut out);
            out.push(Block::Rule);
        } else if let Some(head) = trimmed.strip_prefix('#') {
            flush(&mut para, &mut out);
            out.push(Block::Heading(plain(head.trim_start_matches('#').trim())));
        } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            // a list item stands on its own line
            flush(&mut para, &mut out);
            para.push_str("• ");
            para.push_str(&trimmed[2..]);
            flush(&mut para, &mut out);
        } else {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(trimmed);
        }
    }
    if let Some(lines) = code {
        out.push(Block::Code(lines));
    }
    flush(&mut para, &mut out);
    // a rule right before or after a code block says nothing more
    out.dedup_by(|a, b| *a == Block::Rule && *b == Block::Rule);
    while out.first() == Some(&Block::Rule) {
        out.remove(0);
    }
    while out.last() == Some(&Block::Rule) {
        out.pop();
    }
    out
}

/// markdown marks off a line: `[text](url)` keeps text, `**`, `__` and
/// backticks go.
pub(crate) fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    // `[text](url)`: the `[` that goes with a `](` is the last one before it
    while let Some(mid) = rest.find("](") {
        let Some(open) = rest[..mid].rfind('[') else {
            break;
        };
        let Some(end) = rest[mid..].find(')').map(|i| mid + i) else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..mid]);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    // rust's links to names, `[`Vec`]`, read as the name
    out.replace("[`", "`")
        .replace("`]", "`")
        .replace("**", "")
        .replace("__", "")
        .replace('`', "")
}

/// words of `text` in lines no wider than `width`.
pub(crate) fn wrap(ui: &mut Ui<'_, Id>, text: &str, font: &Font, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let tried = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };
        if !line.is_empty() && ui.text_width(&tried, font) > width {
            lines.push(std::mem::take(&mut line));
            line = word.to_owned();
        } else {
            line = tried;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// a code line in coloured runs: a light reading, not a parse. strings,
/// numbers, comments, keywords, types (capitals), calls.
fn runs(line: &str, dark: bool, plain: Color) -> Vec<(String, Color)> {
    const KEYWORDS: [&str; 34] = [
        "fn",
        "pub",
        "let",
        "mut",
        "const",
        "static",
        "struct",
        "enum",
        "trait",
        "impl",
        "type",
        "use",
        "mod",
        "where",
        "as",
        "dyn",
        "ref",
        "unsafe",
        "async",
        "crate",
        "self",
        "Self",
        "super",
        "def",
        "class",
        "function",
        "var",
        "interface",
        "func",
        "package",
        "import",
        "extends",
        "implements",
        "new",
    ];
    const CONTROL: [&str; 10] = [
        "if", "else", "match", "for", "while", "loop", "return", "await", "break", "continue",
    ];
    let colour = |kind| colour_on(kind, dark);
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<(String, Color)> = Vec::new();
    let mut push = |text: String, c: Color| match out.last_mut() {
        Some((t, last)) if *last == c => t.push_str(&text),
        _ => out.push((text, c)),
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            push(chars[i..].iter().collect(), colour(HighlightKind::Comment));
            break;
        }
        if c == '"' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '"' {
                j += if chars[j] == '\\' { 2 } else { 1 };
            }
            let end = (j + 1).min(chars.len());
            push(
                chars[i..end].iter().collect(),
                colour(HighlightKind::String),
            );
            i = end;
            continue;
        }
        if c == '\'' && chars.get(i + 1).is_some_and(|n| n.is_alphabetic()) {
            // a lifetime: `'a`
            let j = i
                + 1
                + chars[i + 1..]
                    .iter()
                    .take_while(|c| c.is_alphanumeric() || **c == '_')
                    .count();
            push(
                chars[i..j].iter().collect(),
                colour(HighlightKind::Lifetime),
            );
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let j = i + chars[i..]
                .iter()
                .take_while(|c| c.is_alphanumeric() || **c == '_' || **c == '.')
                .count();
            push(chars[i..j].iter().collect(), colour(HighlightKind::Number));
            i = j;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let j = i + chars[i..]
                .iter()
                .take_while(|c| c.is_alphanumeric() || **c == '_')
                .count();
            let word: String = chars[i..j].iter().collect();
            let next = chars[j..].iter().find(|c| !c.is_whitespace());
            let kind = if CONTROL.contains(&word.as_str()) {
                Some(HighlightKind::KeywordControl)
            } else if KEYWORDS.contains(&word.as_str()) {
                Some(HighlightKind::Keyword)
            } else if next == Some(&'!') {
                Some(HighlightKind::Macro)
            } else if next == Some(&'(') {
                Some(HighlightKind::Function)
            } else if word.starts_with(|c: char| c.is_uppercase()) {
                Some(HighlightKind::Type)
            } else {
                None
            };
            push(word, kind.map_or(plain, colour));
            i = j;
            continue;
        }
        push(c.to_string(), plain);
        i += 1;
    }
    out
}

enum Row {
    Code(String),
    Text(String),
    Heading(String),
    Rule,
}

/// draws the card for `text` near `caret`, inside `seen`, faded by `alpha`.
pub fn show(
    ui: &mut Ui<'_, Id>,
    text: &str,
    caret: Rect,
    seen: Rect,
    alpha: f32,
    (dark, size): (bool, f32),
) -> AureaResult<()> {
    let palette: Palette = ui.theme.palette;
    let prose = ui.theme.font(13.0);
    let head = ui.theme.font(13.5);
    let mono = Font::new(MONO, (size - 1.0).clamp(10.0, 16.0));
    let code_row = (mono.size * 1.45).round();
    let text_row = 19.0;
    let max_w = MAX_WIDTH.min(seen.width - 16.0).max(80.0);
    let inner_w = max_w - 2.0 * PAD;

    // the blocks as rows, at most a card's worth
    let mut rows: Vec<Row> = Vec::new();
    let all = blocks(text);
    let count = all.len();
    let mut cut = false;
    for (b, block) in all.into_iter().enumerate() {
        match block {
            Block::Code(lines) => rows.extend(lines.into_iter().map(Row::Code)),
            Block::Text(t) => rows.extend(wrap(ui, &t, &prose, inner_w).into_iter().map(Row::Text)),
            Block::Heading(t) => rows.push(Row::Heading(t)),
            Block::Rule => rows.push(Row::Rule),
        }
        if rows.len() >= MAX_LINES {
            cut = rows.len() > MAX_LINES || b + 1 < count;
            break;
        }
    }
    rows.truncate(MAX_LINES);
    if rows.is_empty() {
        return Ok(());
    }
    let height_of = |row: &Row| match row {
        Row::Code(_) => code_row,
        Row::Rule => 9.0,
        Row::Text(_) | Row::Heading(_) => text_row,
    };
    let mut content_w: f32 = 120.0;
    for row in &rows {
        let w = match row {
            Row::Code(l) => ui.text_width(l, &mono),
            Row::Text(l) => ui.text_width(l, &prose),
            Row::Heading(l) => ui.text_width(l, &head),
            Row::Rule => 0.0,
        };
        content_w = content_w.max(w);
    }
    let width = content_w.min(inner_w) + 2.0 * PAD;
    // each run of code rows gets a little room under its band
    let code_runs = rows
        .iter()
        .zip(rows.iter().skip(1).map(Some).chain([None]))
        .filter(|(row, next)| matches!(row, Row::Code(_)) && !matches!(next, Some(Row::Code(_))))
        .count();
    let height = rows.iter().map(height_of).sum::<f32>()
        + code_runs as f32 * 4.0
        + 2.0 * PAD
        + if cut { text_row } else { 0.0 };

    // under the caret when it fits, else over it
    let below = caret.y + caret.height + 6.0;
    let y = if below + height <= seen.y + seen.height {
        below
    } else {
        (caret.y - height - 6.0).max(seen.y)
    };
    let x = caret.x.min(seen.x + seen.width - width).max(seen.x);
    let card = Rect::new(x, y + (1.0 - alpha) * 6.0, width, height);
    ui.shadow(card, 8.0, 18.0, palette.shadow.fade(0.5 * alpha))?;
    ui.round(card, 8.0, palette.raised.fade(alpha))?;
    ui.border(card, 8.0, 1.0, palette.border.fade(alpha))?;

    let mut at = card.y + PAD;
    let mut i = 0;
    while i < rows.len() {
        // a run of code rows sits on one darker band
        if matches!(rows[i], Row::Code(_)) {
            let n = rows[i..]
                .iter()
                .take_while(|r| matches!(r, Row::Code(_)))
                .count();
            let band = Rect::new(
                card.x + 4.0,
                at - 3.0,
                card.width - 8.0,
                n as f32 * code_row + 6.0,
            );
            ui.round(band, 5.0, palette.surface.fade(alpha))?;
            for row in &rows[i..i + n] {
                if let Row::Code(line) = row {
                    let mut cx = card.x + PAD;
                    for (piece, colour) in runs(line, dark, palette.text) {
                        let w = ui.text_width(&piece, &mono);
                        let room = (card.x + card.width - PAD - cx).max(0.0);
                        let cell = Rect::new(cx, at, room, code_row);
                        ui.text_clipped(&piece, cell, &mono, colour.fade(alpha), Align::Start)?;
                        cx += w;
                    }
                }
                at += code_row;
            }
            at += 4.0;
            i += n;
            continue;
        }
        let row = &rows[i];
        let line = Rect::new(card.x + PAD, at, card.width - 2.0 * PAD, height_of(row));
        match row {
            Row::Text(t) => {
                ui.text_clipped(t, line, &prose, palette.text.fade(alpha), Align::Start)?;
            }
            Row::Heading(t) => {
                ui.text_clipped(t, line, &head, palette.text.fade(alpha), Align::Start)?;
            }
            Row::Rule => {
                let mid = Rect::new(card.x + 6.0, at + 4.0, card.width - 12.0, 1.0);
                ui.fill(mid, palette.border.fade(alpha))?;
            }
            Row::Code(_) => {}
        }
        at += height_of(row);
        i += 1;
    }
    if cut {
        let more = Rect::new(card.x + PAD, at, card.width - 2.0 * PAD, text_row);
        ui.text("…", more, &prose, palette.dim.fade(alpha), Align::Start)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_into_blocks() {
        let md = "```rust\nfn push(&mut self, value: T)\n```\n---\nAppends an **element** to the\nback of a [`Vec`](https://x).\n\n# Panics\n- if full\n";
        let got = blocks(md);
        assert_eq!(
            got,
            [
                Block::Code(vec!["fn push(&mut self, value: T)".into()]),
                Block::Rule,
                Block::Text("Appends an element to the back of a Vec.".into()),
                Block::Heading("Panics".into()),
                Block::Text("• if full".into()),
            ]
        );
        assert!(blocks("").is_empty());
        assert_eq!(plain("[`Option`] or [`Vec`](https://x)"), "Option or Vec");
        assert_eq!(plain("a [b] c"), "a [b] c");
        assert_eq!(
            blocks("```\nopen fence"),
            [Block::Code(vec!["open fence".into()])]
        );
    }

    #[test]
    fn code_lines_in_coloured_runs() {
        let plain = Color::rgb(1, 2, 3);
        let r = runs("pub fn len<'a>(&self) -> usize // n", true, plain);
        let text: String = r.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(text, "pub fn len<'a>(&self) -> usize // n", "nothing lost");
        let colour_of = |word: &str| r.iter().find(|(t, _)| t.contains(word)).map(|(_, c)| *c);
        assert_eq!(
            colour_of("pub"),
            Some(colour_on(HighlightKind::Keyword, true))
        );
        assert_eq!(
            colour_of("'a"),
            Some(colour_on(HighlightKind::Lifetime, true))
        );
        assert_eq!(
            colour_of("// n"),
            Some(colour_on(HighlightKind::Comment, true))
        );
    }
}
