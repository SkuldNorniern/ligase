//! source view and editing. draws only the lines in sight; text comes from
//! the buffer snapshot, never a copy of the whole file.

pub mod cursor;
pub mod input;

use std::ops::Range;

use aurea::AureaResult;
use aurea::render::{Font, Point, Rect};
use aurea::ui::{Align, ScrollArea, Sense, Ui};
use resolvase::TextSnapshot;

use crate::app::Id;
use crate::document::{Document, Documents, Load, View, ViewId};
use crate::lang::Texts;
use cursor::Selection;
use input::Action;

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
            // typing after a move is a new undo step
            doc.buffer.break_join();
            return true;
        }
        Action::Collapse => {
            v.sel = Selection::caret(sel.head);
            return true;
        }
        Action::Preedit(composing, at) => {
            v.preedit = (!composing.is_empty()).then_some((composing, at));
            v.reveal = true;
            return true;
        }
        Action::Insert(typed) => {
            v.preedit = None;
            ((sel.range(), typed), true)
        }
        Action::Indent => ((sel.range(), TAB.to_owned()), true),
        Action::Newline => {
            let start = sel.range().start;
            let line = format!(
                "{}{}",
                doc.line_break(),
                cursor::indent_before(&text, start)
            );
            ((sel.range(), line), false)
        }
        Action::Backspace if sel.is_empty() => {
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
}

/// one line laid out for drawing: tabs opened up, with where each char
/// sits in the buffer and in the drawn text.
struct Laid {
    drawn: String,
    /// (buffer offset, drawn offset) per char, plus one past the end.
    marks: Vec<(usize, usize)>,
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
            '\r' => {}
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
) -> AureaResult<Shown> {
    let mut shown = Shown {
        focused: false,
        caret: None,
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
    let caret_line = text.line_of(view.sel.head).unwrap_or(0);
    if std::mem::take(&mut view.reveal) {
        let y = caret_line as f32 * LINE;
        if y < view.scroll {
            view.scroll = y;
        } else if y + LINE > view.scroll + rect.height {
            view.scroll = y + LINE - rect.height;
        }
    }
    let mut sel = view.sel;
    let preedit = view.preedit.clone();
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
                let range = text.line_range(line).unwrap_or(0..0);
                lay(text, range).offset_at(ui, &font, at.x - body_x)
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
            } else if input.held
                && let Some(at) = ui.pointer()
            {
                sel.head = hit_test(ui, at);
            }

            let range = sel.range();
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
                let Ok(span) = text.line_range(line) else {
                    continue;
                };
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
                ui.text(&laid.drawn, body, &font, palette.text, Align::Start)?;
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
            Ok(())
        },
    )?;
    view.sel = sel;
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
