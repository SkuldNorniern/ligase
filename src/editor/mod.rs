//! source view and editing. draws only the lines in sight; text comes from
//! the buffer snapshot, never a copy of the whole file.

pub mod cursor;
pub mod input;
pub mod lines;
pub mod pairs;

use std::ops::Range;

use aurea::AureaResult;
use aurea::render::{Color, Font, Point, Rect};
use aurea::ui::{Align, ScrollArea, Sense, Ui};
use resolvase::TextSnapshot;

use crate::app::Id;
use crate::document::{Document, Documents, Load, View, ViewId};
use crate::lang::Texts;
use crate::syntax::{self, Spans};
use cursor::Selection;
use input::Action;
use lines::Change;

const SIZE: f32 = 14.0;
const LINE: f32 = 20.0;
/// longest part of a line drawn; past this it is cut for now.
const MAX_DRAWN: usize = 4096;
/// room the scroll bar takes on the right; clicks there are its.
const BAR: f32 = 14.0;
const TAB: &str = "    ";

const MONO: &str = if cfg!(windows) {
    "Consolas"
} else if cfg!(target_os = "macos") {
    "Menlo"
} else {
    "DejaVu Sans Mono"
};

/// does `action` on `view`. true when something changed.
pub fn act(docs: &mut Documents, view: ViewId, action: Action) -> bool {
    let Some((doc, v)) = docs.editable(view) else {
        return false;
    };
    let text = doc.buffer.snapshot();
    let sel = v.sel;
    let (edit, join) = match action {
        Action::Move(motion, extend) => {
            v.sel = cursor::motion(&text, sel, motion, extend, &mut v.goal);
            v.reveal = true;
            v.preedit = None;
            // typing after a move is a new undo step
            doc.buffer.break_join();
            return true;
        }
        Action::Collapse => {
            v.sel = Selection::caret(sel.head);
            v.preedit = None;
            return true;
        }
        Action::Preedit(composing, at) => {
            v.preedit = (!composing.is_empty()).then_some((composing, at));
            v.reveal = true;
            return true;
        }
        Action::Insert(typed) => {
            v.preedit = None;
            if let Some(change) = pairs::typed(&text, sel, &typed) {
                return apply(docs, view, change, true);
            }
            ((sel.range(), typed), true)
        }
        Action::Indent if lines::block(&text, sel).0 != lines::block(&text, sel).1 => {
            let change = lines::indent_lines(&text, sel, TAB);
            return apply(docs, view, change, false);
        }
        Action::Indent => ((sel.range(), TAB.to_owned()), true),
        Action::Outdent => {
            let change = lines::outdent_lines(&text, sel, TAB);
            return apply(docs, view, change, false);
        }
        Action::Newline => {
            let start = sel.range().start;
            let lb = doc.line_break();
            let indent = cursor::indent_before(&text, start);
            let open = pairs::char_before(&text, start);
            let close = pairs::char_after(&text, sel.range().end);
            let opens = matches!(open, Some('(' | '[' | '{'))
                || (open == Some(':') && doc.language == Some("python"));
            if opens && open.and_then(pairs::closer) == close {
                // between a pair: the caret gets its own line, deeper
                let first = format!("{lb}{indent}{TAB}");
                let change = Change {
                    sel: Selection::caret(start + first.len()),
                    edits: vec![(sel.range(), format!("{first}{lb}{indent}"))],
                };
                return apply(docs, view, change, false);
            }
            let deeper = if opens { TAB } else { "" };
            ((sel.range(), format!("{lb}{indent}{deeper}")), false)
        }
        Action::OpenLine(above) => {
            let line = text.line_of(sel.head).unwrap_or(0);
            let lb = doc.line_break();
            let indent = cursor::indent_before(&text, cursor::line_end(&text, line));
            let change = if above {
                let at = text.line_start(line).unwrap_or(0);
                Change {
                    sel: Selection::caret(at + indent.len()),
                    edits: vec![(at..at, format!("{indent}{lb}"))],
                }
            } else {
                let at = cursor::line_end(&text, line);
                Change {
                    sel: Selection::caret(at + lb.len() + indent.len()),
                    edits: vec![(at..at, format!("{lb}{indent}"))],
                }
            };
            return apply(docs, view, change, false);
        }
        Action::DeleteWord(forward) if sel.is_empty() => {
            let motion = if forward {
                cursor::Motion::WordRight
            } else {
                cursor::Motion::WordLeft
            };
            let to = cursor::motion(&text, sel, motion, false, &mut None).head;
            ((to.min(sel.head)..to.max(sel.head), String::new()), false)
        }
        Action::DeleteWord(_) => ((sel.range(), String::new()), false),
        Action::Backspace if sel.is_empty() => {
            if let Some(change) = pairs::backspace(&text, sel.head) {
                return apply(docs, view, change, true);
            }
            // in the indent, back to the last tab stop
            let line = text.line_of(sel.head).unwrap_or(0);
            let start = text.line_start(line).unwrap_or(0);
            let before = text.slice(start..sel.head).unwrap_or_default();
            if !before.is_empty() && before.bytes().all(|b| b == b' ') {
                let take = match before.len() % TAB.len() {
                    0 => TAB.len(),
                    n => n,
                };
                return docs.edit(view, vec![(sel.head - take..sel.head, String::new())], true);
            }
            let Some(prev) = text.prev_grapheme(sel.head).ok().flatten() else {
                return false;
            };
            ((prev..sel.head, String::new()), true)
        }
        Action::Delete if sel.is_empty() => {
            let Some(next) = text.next_grapheme(sel.head).ok().flatten() else {
                return false;
            };
            ((sel.head..next, String::new()), true)
        }
        Action::Backspace | Action::Delete => ((sel.range(), String::new()), false),
    };
    docs.edit(view, vec![edit], join)
}

/// makes `change` and puts its selection on the view.
pub fn apply(docs: &mut Documents, view: ViewId, change: Change, join: bool) -> bool {
    let done = change.edits.is_empty() || docs.edit(view, change.edits, join);
    if done && let Some(v) = docs.view_mut(view) {
        v.sel = change.sel;
        v.goal = None;
        v.reveal = true;
    }
    done
}

/// a line edit the app asks for by command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineOp {
    Move(bool),
    Copy(bool),
    Delete,
    Comment,
    Select,
}

/// does `op` on the lines under the selection. true when something changed.
pub fn line_op(docs: &mut Documents, view: ViewId, op: LineOp) -> bool {
    let Some((doc, v)) = docs.editable(view) else {
        return false;
    };
    let text = doc.buffer.snapshot();
    let lb = doc.line_break();
    let sel = v.sel;
    let change = match op {
        LineOp::Move(up) => lines::move_lines(&text, sel, up, lb),
        LineOp::Copy(up) => Some(lines::copy_lines(&text, sel, up, lb)),
        LineOp::Delete => Some(lines::delete_lines(&text, sel)),
        LineOp::Comment => lines::comment_for(doc.language)
            .and_then(|mark| lines::toggle_comment(&text, sel, mark)),
        LineOp::Select => {
            // again and again takes the next line too
            let (a, b) = lines::block(&text, sel);
            let from = text.line_start(a).unwrap_or(0);
            let to = cursor::line_at(&text, text.line_start(b).unwrap_or(0)).head;
            let to = if sel.range() == (from..to) {
                cursor::line_at(&text, to).head
            } else {
                to
            };
            v.sel = Selection {
                anchor: from,
                head: to,
            };
            v.reveal = true;
            return true;
        }
    };
    // each line edit is its own undo step
    doc.buffer.break_join();
    change.is_some_and(|c| apply(docs, view, c, false))
}

/// the whole line under the caret with its break; what copy and cut take
/// when nothing is selected.
pub fn caret_line(docs: &Documents, view: ViewId) -> Option<(Range<usize>, String)> {
    let v = docs.view(view)?;
    let doc = docs.doc(v.doc)?;
    let text = doc.buffer.text();
    let range = cursor::line_at(text, v.sel.head).range();
    let mut line = text.slice(range.clone()).ok()?;
    if !line.ends_with('\n') {
        line.push_str(doc.line_break());
    }
    Some((range, line))
}

/// pastes a whole line copied with nothing selected: it goes in above the
/// caret's line, the caret stays where it was in the text.
pub fn paste_line(docs: &mut Documents, view: ViewId, line: &str) -> bool {
    let Some(v) = docs.view(view) else {
        return false;
    };
    let Some(doc) = docs.doc(v.doc) else {
        return false;
    };
    let text = doc.buffer.text();
    let at = text
        .line_start(text.line_of(v.sel.head).unwrap_or(0))
        .unwrap_or(0);
    let sel = Selection::caret(v.sel.head + line.len());
    apply(
        docs,
        view,
        Change {
            edits: vec![(at..at, line.to_owned())],
            sel,
        },
        false,
    )
}

/// the selected text of `view`.
pub fn selected(docs: &Documents, view: ViewId) -> Option<String> {
    let v = docs.view(view)?;
    let doc = docs.doc(v.doc)?;
    let range = v.sel.range();
    (!range.is_empty())
        .then(|| doc.buffer.text().slice(range).ok())
        .flatten()
}

/// pastes `text` over the selection, line breaks made to match the file.
pub fn paste(docs: &mut Documents, view: ViewId, text: &str) -> bool {
    let Some((doc, v)) = docs.editable(view) else {
        return false;
    };
    let plain = text.replace("\r\n", "\n");
    let fitted = if doc.crlf {
        plain.replace('\n', "\r\n")
    } else {
        plain
    };
    let range = v.sel.range();
    docs.edit(view, vec![(range, fitted)], false)
}

pub fn select_all(docs: &mut Documents, view: ViewId) {
    if let Some((doc, v)) = docs.editable(view) {
        v.sel = Selection {
            anchor: 0,
            head: doc.buffer.len(),
        };
    }
}

/// what a frame of the view tells the app.
pub struct Shown {
    /// the view has the keyboard.
    pub focused: bool,
    /// caret box in window space, for the ime window.
    pub caret: Option<Rect>,
    /// the mouse placed the caret; typing after it is a new undo step.
    pub pressed: bool,
}

/// one line laid out for drawing: tabs opened up, with where each char
/// sits in the buffer and in the drawn text.
struct Laid {
    drawn: String,
    /// (buffer offset, drawn offset) per char, plus one past the end.
    marks: Vec<(usize, usize)>,
}

/// draws one line in runs: plain text between spans, each span in its colour.
fn paint_line(
    ui: &mut Ui<'_, Id>,
    font: &Font,
    laid: &Laid,
    spans: &Spans,
    line: Range<usize>,
    body: Rect,
    plain: Color,
) -> AureaResult<()> {
    let run = |ui: &mut Ui<'_, Id>, from: usize, to: usize, color: Color| {
        let (a, b) = (laid.drawn_at(from), laid.drawn_at(to));
        if a >= b {
            return Ok(());
        }
        let x = ui.text_width(&laid.drawn[..a], font);
        let at = Rect::new(body.x + x, body.y, (body.width - x).max(0.0), body.height);
        ui.text(&laid.drawn[a..b], at, font, color, Align::Start)
    };
    // the span just before the line can reach into it, like a block comment
    let first = spans
        .partition_point(|(r, _)| r.start < line.start)
        .saturating_sub(1);
    let mut at = line.start;
    for (range, kind) in &spans[first..] {
        if range.start >= line.end {
            break;
        }
        let from = range.start.max(at);
        let to = range.end.min(line.end);
        if from >= to {
            continue;
        }
        run(ui, at, from, plain)?;
        run(ui, from, to, syntax::colour(*kind))?;
        at = to;
    }
    run(ui, at, line.end, plain)
}

/// wavy underlines under the diagnostics that touch this line.
fn squiggles(
    ui: &mut Ui<'_, Id>,
    font: &Font,
    laid: &Laid,
    diags: &[crate::lsp::Diag],
    line: Range<usize>,
    body: Rect,
) -> AureaResult<()> {
    use crate::lsp::Severity;
    let palette = ui.theme.palette;
    for diag in diags {
        if diag.range.start > line.end || diag.range.end < line.start {
            continue;
        }
        let from = diag.range.start.max(line.start);
        let to = diag.range.end.min(line.end);
        let x0 = body.x + laid.x_of(ui, font, from);
        // an empty range still gets a mark the width of one char
        let x1 = (body.x + laid.x_of(ui, font, to)).max(x0 + ui.text_width("m", font));
        let color = match diag.severity {
            Severity::Error => palette.danger,
            Severity::Warning => palette.warning,
            Severity::Info => palette.accent,
            Severity::Hint => palette.dim,
        };
        let y = body.y + LINE - 3.0;
        let mut points = Vec::new();
        let mut x = x0;
        let mut up = false;
        while x < x1 {
            points.push(Point::new(x, if up { y - 1.5 } else { y + 1.5 }));
            x += 3.0;
            up = !up;
        }
        points.push(Point::new(x1, y));
        ui.polyline(&points, 1.0, color)?;
    }
    Ok(())
}

/// language server hover text in a box under the caret, or over it when
/// there is no room below.
fn hover_box(ui: &mut Ui<'_, Id>, text: &str, caret: Rect, seen: Rect) -> AureaResult<()> {
    const MAX_LINES: usize = 16;
    let palette = ui.theme.palette;
    let font = ui.theme.font(13.0);
    let lines: Vec<&str> = text.lines().take(MAX_LINES).collect();
    let row = 18.0;
    let width = lines
        .iter()
        .map(|l| ui.text_width(l, &font))
        .fold(120.0_f32, f32::max)
        .min(seen.width * 0.8)
        + 16.0;
    let height = lines.len() as f32 * row + 12.0;
    let below = caret.y + caret.height + 4.0;
    let y = if below + height <= seen.y + seen.height {
        below
    } else {
        (caret.y - height - 4.0).max(seen.y)
    };
    let x = caret.x.min(seen.x + seen.width - width).max(seen.x);
    let card = Rect::new(x, y, width, height);
    ui.fill(card, palette.raised)?;
    ui.border(card, 4.0, 1.0, palette.border)?;
    for (i, line) in lines.iter().enumerate() {
        let at = Rect::new(x + 8.0, y + 6.0 + i as f32 * row, width - 16.0, row);
        ui.text_clipped(line, at, &font, palette.text, Align::Start)?;
    }
    Ok(())
}

/// a line's text without its line break, `\r` included, so no click or
/// drag can land between `\r` and `\n`.
fn span(text: &TextSnapshot, line: usize) -> Range<usize> {
    text.line_start(line).unwrap_or(0)..cursor::line_end(text, line)
}

fn lay(text: &TextSnapshot, range: Range<usize>) -> Laid {
    let mut end = range.end.min(range.start + MAX_DRAWN);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut laid = Laid {
        drawn: String::new(),
        marks: Vec::new(),
    };
    let mut at = range.start;
    for c in text
        .chunks(range.start..end)
        .into_iter()
        .flatten()
        .flat_map(str::chars)
    {
        laid.marks.push((at, laid.drawn.len()));
        match c {
            '\t' => laid.drawn.push_str(TAB),
            c => laid.drawn.push(c),
        }
        at += c.len_utf8();
    }
    laid.marks.push((at, laid.drawn.len()));
    laid
}

impl Laid {
    fn drawn_at(&self, offset: usize) -> usize {
        self.marks
            .iter()
            .rev()
            .find(|(b, _)| *b <= offset)
            .map_or(0, |(_, d)| *d)
    }

    fn x_of(&self, ui: &mut Ui<'_, Id>, font: &Font, offset: usize) -> f32 {
        ui.text_width(&self.drawn[..self.drawn_at(offset)], font)
    }

    /// buffer offset closest to `x`.
    fn offset_at(&self, ui: &mut Ui<'_, Id>, font: &Font, x: f32) -> usize {
        // marks are in order, so their widths are too; find the first past x
        let (mut lo, mut hi) = (0, self.marks.len() - 1);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if ui.text_width(&self.drawn[..self.marks[mid].1], font) < x {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo > 0 {
            let before = ui.text_width(&self.drawn[..self.marks[lo - 1].1], font);
            let after = ui.text_width(&self.drawn[..self.marks[lo].1], font);
            if x - before < after - x {
                return self.marks[lo - 1].0;
            }
        }
        self.marks[lo].0
    }
}

pub fn show(
    ui: &mut Ui<'_, Id>,
    id: ViewId,
    doc: &Document,
    view: &mut View,
    rect: Rect,
    texts: &Texts,
    hover: Option<&str>,
) -> AureaResult<Shown> {
    let mut shown = Shown {
        focused: false,
        caret: None,
        pressed: false,
    };
    let font = Font::new(MONO, SIZE);
    let palette = ui.theme.palette;
    match &doc.load {
        Load::Loading => {
            ui.text(texts.tr("loading"), rect, &font, palette.dim, Align::Center)?;
            return Ok(shown);
        }
        Load::Failed(why) => {
            let line = format!("{}: {why}", texts.tr("can't open"));
            ui.text(&line, rect, &font, palette.danger, Align::Center)?;
            return Ok(shown);
        }
        Load::Ready => {}
    }
    let text = doc.buffer.text();
    let lines = text.line_count();
    let digits = lines.to_string().len().max(3);
    let gutter = ui.text_width(&"0".repeat(digits), &font) + 16.0;
    let height = lines as f32 * LINE + LINE;
    view.page = ((rect.height / LINE) as usize).saturating_sub(1).max(1);
    if let Some((at, encoding)) = view.goto.take() {
        // past the end or mid char: lands on the nearest place before it
        view.sel = Selection::caret(crate::lsp::offset_near(text, at, encoding));
        view.reveal = true;
    }
    if std::mem::take(&mut view.reveal) {
        let y = text.line_of(view.sel.head).unwrap_or(0) as f32 * LINE;
        if y < view.scroll {
            view.scroll = y;
        } else if y + LINE > view.scroll + rect.height {
            view.scroll = y + LINE - rect.height;
        }
    }
    let mut sel = view.sel;
    let mut preedit = view.preedit.clone();
    ScrollArea::new(&mut view.scroll, height).show(
        ui,
        Id::Editor(id),
        rect,
        |ui, content, seen| {
            let first = ((seen.y - content.y) / LINE).floor().max(0.0) as usize;
            let count = (seen.height / LINE).ceil() as usize + 1;
            let body_x = content.x + gutter + 8.0;

            // pointer: press places the caret, drag stretches it
            let hit = Rect::new(seen.x, seen.y, (seen.width - BAR).max(0.0), seen.height);
            let input = ui.interact(Id::Editor(id), 1, hit, Sense::Click, true);
            shown.focused = input.focused;
            let hit_test = |ui: &mut Ui<'_, Id>, at: Point| {
                let line = ((at.y - content.y) / LINE)
                    .floor()
                    .clamp(0.0, (lines - 1) as f32) as usize;
                lay(text, span(text, line)).offset_at(ui, &font, at.x - body_x)
            };
            if let Some(press) = ui.press(Id::Editor(id), 1) {
                let at = hit_test(ui, press.at);
                sel = match press.clicks {
                    2 => cursor::word_at(text, at),
                    n if n >= 3 => cursor::line_at(text, at),
                    _ if press.shift => Selection {
                        anchor: sel.anchor,
                        head: at,
                    },
                    _ => Selection::caret(at),
                };
                // a click ends composing and the typing run
                preedit = None;
                shown.pressed = true;
            } else if input.held
                && let Some(at) = ui.pointer()
            {
                sel.head = hit_test(ui, at);
            }

            let range = sel.range();
            let caret_line = text.line_of(sel.head).unwrap_or(0);
            for line in first..(first + count).min(lines) {
                let y = content.y + line as f32 * LINE;
                let row = Rect::new(content.x, y, content.width, LINE);
                let (number, body) = row.split_left(gutter, 8.0);
                ui.text(
                    &(line + 1).to_string(),
                    number,
                    &font,
                    palette.faint,
                    Align::End,
                )?;
                let span = span(text, line);
                let laid = lay(text, span.clone());
                // selection behind the text, one extra space when it runs past the line end
                let from = range.start.max(span.start);
                let to = range.end.min(span.end);
                if from < to || (range.start <= span.end && range.end > span.end) {
                    let x0 = laid.x_of(ui, &font, from);
                    let mut x1 = laid.x_of(ui, &font, to);
                    if range.end > span.end {
                        x1 += ui.text_width(" ", &font);
                    }
                    ui.fill(Rect::new(body.x + x0, y, x1 - x0, LINE), palette.selection)?;
                }
                paint_line(
                    ui,
                    &font,
                    &laid,
                    &doc.spans,
                    span.clone(),
                    body,
                    palette.text,
                )?;
                squiggles(ui, &font, &laid, &doc.diags, span, body)?;
                if line == caret_line && shown.focused {
                    let x = body.x + laid.x_of(ui, &font, sel.head);
                    let mut caret_x = x;
                    if let Some((composing, at)) = &preedit {
                        let width = ui.text_width(composing, &font);
                        let box_ = Rect::new(x, y, width, LINE);
                        ui.fill(box_, palette.raised)?;
                        ui.text(composing, box_, &font, palette.text, Align::Start)?;
                        ui.fill(Rect::new(x, y + LINE - 2.0, width, 1.0), palette.text)?;
                        let inside = at.map_or(composing.len(), |c| c.min(composing.len()));
                        let inside = (0..=inside)
                            .rev()
                            .find(|i| composing.is_char_boundary(*i))
                            .unwrap_or(0);
                        caret_x = x + ui.text_width(&composing[..inside], &font);
                    }
                    ui.fill(Rect::new(caret_x, y + 2.0, 2.0, LINE - 4.0), palette.text)?;
                    shown.caret = Some(Rect::new(caret_x, y, 2.0, LINE));
                }
            }
            if let (Some(text), Some(caret)) = (hover, shown.caret) {
                hover_box(ui, text, caret, seen)?;
            }
            Ok(())
        },
    )?;
    view.sel = sel;
    view.preedit = preedit;
    Ok(shown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::cursor::Motion;
    use std::path::Path;

    fn open(text: &str) -> (Documents, ViewId) {
        let mut docs = Documents::default();
        let (doc, _) = docs.open(Path::new("t.rs"));
        let view = docs.add_view(doc);
        docs.loaded(doc, Ok(text.to_owned()));
        (docs, view)
    }

    fn text(docs: &Documents, view: ViewId) -> String {
        docs.doc_of(view).unwrap().buffer.text().to_string()
    }

    fn caret(docs: &mut Documents, view: ViewId, at: usize) {
        docs.view_mut(view).unwrap().sel = Selection::caret(at);
    }

    #[test]
    fn typing_backspace_and_undo() {
        let (mut docs, v) = open("");
        for c in ["가", "e\u{301}", "!"] {
            act(&mut docs, v, Action::Insert(c.into()));
        }
        assert_eq!(text(&docs, v), "가e\u{301}!");
        act(&mut docs, v, Action::Backspace);
        act(&mut docs, v, Action::Backspace);
        assert_eq!(text(&docs, v), "가", "accent goes with its letter");
        // the whole typing run is one step
        assert!(docs.undo(v));
        assert_eq!(text(&docs, v), "");
    }

    #[test]
    fn crlf_lines_lay_out_without_cr() {
        let (docs, v) = open("ab\r\ncd");
        let text = docs.doc_of(v).unwrap().buffer.text();
        assert_eq!(span(text, 0), 0..2);
        let laid = lay(text, span(text, 0));
        assert_eq!(laid.drawn, "ab");
        assert_eq!(laid.marks.last(), Some(&(2, 2)));
    }

    #[test]
    fn newline_keeps_indent_and_crlf() {
        let (mut docs, v) = open("    let x;\r\n");
        caret(&mut docs, v, 10);
        act(&mut docs, v, Action::Newline);
        assert_eq!(text(&docs, v), "    let x;\r\n    \r\n");
        assert_eq!(docs.view(v).unwrap().sel, Selection::caret(16));
    }

    #[test]
    fn paste_fits_line_breaks() {
        let (mut docs, v) = open("a\r\n");
        caret(&mut docs, v, 1);
        paste(&mut docs, v, "x\ny\r\nz");
        assert_eq!(text(&docs, v), "ax\r\ny\r\nz\r\n");
    }

    #[test]
    fn selection_replaced_and_cut() {
        let (mut docs, v) = open("hello world");
        docs.view_mut(v).unwrap().sel = Selection {
            anchor: 6,
            head: 11,
        };
        assert_eq!(selected(&docs, v).as_deref(), Some("world"));
        act(&mut docs, v, Action::Insert("there".into()));
        assert_eq!(text(&docs, v), "hello there");
        select_all(&mut docs, v);
        act(&mut docs, v, Action::Delete);
        assert_eq!(text(&docs, v), "");
    }

    #[test]
    fn enter_between_braces_opens_a_line() {
        let (mut docs, v) = open("  f {}");
        caret(&mut docs, v, 5);
        act(&mut docs, v, Action::Newline);
        assert_eq!(text(&docs, v), "  f {\n      \n  }");
        assert_eq!(docs.view(v).unwrap().sel, Selection::caret(12));
    }

    #[test]
    fn backspace_in_indent_goes_to_tab_stop() {
        let (mut docs, v) = open("      x");
        caret(&mut docs, v, 6);
        act(&mut docs, v, Action::Backspace);
        assert_eq!(text(&docs, v), "    x");
        act(&mut docs, v, Action::Backspace);
        assert_eq!(text(&docs, v), "x");
    }

    #[test]
    fn typing_pairs_and_word_delete() {
        let (mut docs, v) = open("");
        act(&mut docs, v, Action::Insert("(".into()));
        act(&mut docs, v, Action::Insert("a".into()));
        act(&mut docs, v, Action::Insert(")".into()));
        assert_eq!(text(&docs, v), "(a)");
        assert_eq!(docs.view(v).unwrap().sel, Selection::caret(3));
        act(&mut docs, v, Action::Insert(" word".into()));
        act(&mut docs, v, Action::DeleteWord(false));
        assert_eq!(text(&docs, v), "(a) ");
    }

    #[test]
    fn line_copy_pastes_above() {
        let (mut docs, v) = open("a\nb");
        caret(&mut docs, v, 3);
        let (_, line) = caret_line(&docs, v).unwrap();
        assert_eq!(line, "b\n");
        paste_line(&mut docs, v, &line);
        assert_eq!(text(&docs, v), "a\nb\nb");
        assert_eq!(docs.view(v).unwrap().sel, Selection::caret(5));
    }

    #[test]
    fn move_breaks_typing_run() {
        let (mut docs, v) = open("ab");
        caret(&mut docs, v, 2);
        act(&mut docs, v, Action::Insert("c".into()));
        act(&mut docs, v, Action::Move(Motion::DocStart, false));
        act(&mut docs, v, Action::Insert("z".into()));
        docs.undo(v);
        assert_eq!(text(&docs, v), "abc");
    }
}
