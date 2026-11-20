//! find and replace in one file, and go to line. a small bar over the top
//! right of the editor; matches are found on the worker.

use std::ops::Range;

use aurea::AureaResult;
use aurea::render::Rect;
use aurea::ui::{Align, Button, Channel, TextField, Ui};
use resolvase::{Revision, TextSnapshot};

use crate::app::Id;
use crate::document::{DocumentId, ViewId};
use crate::lang::Texts;

/// matches kept per file; past this the count says so.
pub const MAX_HITS: usize = 20_000;
const ROW: f32 = 30.0;
const PAD: f32 = 6.0;
const WIDTH: f32 = 440.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Find,
    Replace,
    Goto,
    /// a new name for the name at the caret.
    Rename,
}

/// every match of `query` in `text` with `opts`, at most `MAX_HITS`.
/// runs on the worker.
pub fn find_all(text: &TextSnapshot, query: &str, opts: crate::search::Opts) -> Vec<Range<usize>> {
    let mut hits = Vec::new();
    if query.is_empty() {
        return hits;
    }
    let matcher = crate::search::matcher_with(query, opts);
    // one walk over the text: a lookup per line costs about 15x more
    for (start, s) in text.lines() {
        let range = start..start + s.len();
        let mut from = 0;
        while let Some((at, len)) = matcher(&s[from..]) {
            let start = range.start + from + at;
            hits.push(start..start + len);
            if hits.len() >= MAX_HITS {
                return hits;
            }
            // an empty match can't happen, but never loop on one
            from += at + len.max(1);
            while from < s.len() && !s.is_char_boundary(from) {
                from += 1;
            }
            if from >= s.len() {
                break;
            }
        }
    }
    hits
}

/// "12:4" or "12" to a zero based line and column.
pub fn parse_goto(text: &str) -> Option<(usize, usize)> {
    let mut parts = text.trim().splitn(2, [':', ',']);
    let line: usize = parts.next()?.trim().parse().ok()?;
    let column: usize = match parts.next() {
        Some(c) if !c.trim().is_empty() => c.trim().parse().ok()?,
        _ => 1,
    };
    Some((line.max(1) - 1, column.max(1) - 1))
}

/// the bar and what it found.
#[derive(Default)]
pub struct Bar {
    pub mode: Option<Mode>,
    pub view: Option<ViewId>,
    pub query: String,
    pub with: String,
    pub goto: String,
    pub rename: String,
    /// matches for (document, revision, query).
    pub hits: Vec<Range<usize>>,
    pub found_for: Option<(DocumentId, Revision, String)>,
    pub asked: Option<(DocumentId, Revision, String)>,
    /// the match shown as picked.
    pub current: Option<usize>,
    /// jump to the first match after this once matches come in.
    pub jump_from: Option<usize>,
    /// next, back or replace asked while matches were on the way.
    pub pending: Option<Ask>,
}

/// what the bar asks the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    Next,
    Prev,
    Replace,
    ReplaceAll,
    Goto,
    Rename,
    Close,
    /// the query changed; find again and jump.
    Changed,
    ToggleReplace,
}

impl Bar {
    pub fn is_open(&self) -> bool {
        self.mode.is_some()
    }

    /// matches for the text as it is now, or none while they are stale.
    pub fn fresh(&self, doc: DocumentId, revision: Revision) -> &[Range<usize>] {
        match &self.found_for {
            Some((d, r, q)) if *d == doc && *r == revision && *q == self.query => &self.hits,
            _ => &[],
        }
    }

    /// index of the first match at or after `at`, wrapping round.
    pub fn after(&self, hits: &[Range<usize>], at: usize) -> Option<usize> {
        if hits.is_empty() {
            return None;
        }
        Some(hits.partition_point(|h| h.start < at) % hits.len())
    }

    /// index of the last match before `at`, wrapping round.
    pub fn before(&self, hits: &[Range<usize>], at: usize) -> Option<usize> {
        if hits.is_empty() {
            return None;
        }
        let i = hits.partition_point(|h| h.start < at);
        Some(i.checked_sub(1).unwrap_or(hits.len() - 1))
    }
}

/// draws the bar over the top right of `editor`. `count` is the match
/// text, like "3 of 12".
pub fn show(
    ui: &mut Ui<'_, Id>,
    bar: &mut Bar,
    editor: Rect,
    count: &str,
    texts: &Texts,
) -> AureaResult<Option<Ask>> {
    let Some(mode) = bar.mode else {
        return Ok(None);
    };
    let palette = ui.theme.palette;
    let rows = if mode == Mode::Replace { 2.0 } else { 1.0 };
    // wide enough for the buttons, unless the editor itself is narrower
    let width = WIDTH
        .min(editor.width - 24.0)
        .max(230.0_f32.min(editor.width - 8.0))
        .max(120.0);
    let height = rows * ROW + (rows + 1.0) * PAD;
    // slides down from under the tab bar as it opens
    let shown = ui.animate_from(Id::FindBar, 0, Channel::Custom(0), 0.0, 1.0, 16.0);
    let card = Rect::new(
        editor.x + editor.width - width - 18.0,
        editor.y + 4.0 - (1.0 - shown) * 10.0,
        width,
        height,
    );
    ui.round(card, 6.0, palette.raised.fade(shown))?;
    ui.border(card, 6.0, 1.0, palette.border.fade(shown))?;
    let inner = card.inset(PAD);
    let (top, rest) = inner.split_top(ROW, PAD);
    let mut ask = None;
    let font = ui.theme.font(12.0);
    let small = |ui: &mut Ui<'_, Id>, label: &str, part: u16, rect: Rect| {
        Button::new(label)
            .quiet()
            .show(ui, Id::FindButton(part), rect)
    };
    let (close, top) = top.split_right(ROW, 2.0);
    if small(ui, "×", 0, close)?.clicked {
        ask = Some(Ask::Close);
    }
    if mode == Mode::Goto || mode == Mode::Rename {
        let (field, hint, done, max) = if mode == Mode::Goto {
            (
                &mut bar.goto,
                texts.tr("Go to line or line:column"),
                Ask::Goto,
                32,
            )
        } else {
            (&mut bar.rename, texts.tr("new name"), Ask::Rename, 200)
        };
        let typed = TextField::new(field)
            .placeholder(hint)
            .max_chars(max)
            .show(ui, Id::FindField, top)?;
        if typed.submitted {
            ask = Some(done);
        }
        if typed.cancelled {
            ask = Some(Ask::Close);
        }
        return Ok(ask);
    }
    let (toggle, top) = top.split_right(ROW, 2.0);
    let (down, top) = top.split_right(ROW, 2.0);
    let (up, top) = top.split_right(ROW, 2.0);
    // the count gives way first when there is no room for the field
    let note_w = if top.width >= 72.0 + 100.0 { 72.0 } else { 0.0 };
    let (note, field) = top.split_right(note_w, 4.0);
    let toggled = Button::new("↔")
        .quiet()
        .selected(mode == Mode::Replace)
        .show(ui, Id::FindButton(1), toggle)?;
    if toggled.clicked {
        ask = Some(Ask::ToggleReplace);
    }
    if small(ui, "↓", 2, down)?.clicked {
        ask = Some(Ask::Next);
    }
    if small(ui, "↑", 3, up)?.clicked {
        ask = Some(Ask::Prev);
    }
    if note.width > 0.0 {
        ui.text_clipped(count, note, &font, palette.dim, Align::Center)?;
    }
    let typed = TextField::new(&mut bar.query)
        .placeholder(texts.tr("find"))
        .max_chars(500)
        .show(ui, Id::FindField, field)?;
    if typed.changed {
        ask = Some(Ask::Changed);
    }
    if typed.submitted {
        ask = Some(Ask::Next);
    }
    if typed.cancelled {
        ask = Some(Ask::Close);
    }
    if mode == Mode::Replace {
        let (row, _) = rest.split_top(ROW, 0.0);
        let (all, row) = row.split_right(56.0, 2.0);
        let (one, field) = row.split_right(72.0, 4.0);
        let typed = TextField::new(&mut bar.with)
            .placeholder(texts.tr("replace with"))
            .max_chars(500)
            .show(ui, Id::ReplaceField, field)?;
        if typed.submitted {
            ask = Some(Ask::Replace);
        }
        if typed.cancelled {
            ask = Some(Ask::Close);
        }
        if small(ui, texts.tr("Replace"), 4, one)?.clicked {
            ask = Some(Ask::Replace);
        }
        if small(ui, texts.tr("All"), 5, all)?.clicked {
            ask = Some(Ask::ReplaceAll);
        }
    }
    Ok(ask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use resolvase::TextBuffer;

    #[test]
    fn finds_every_match_smart_case() {
        let t = TextBuffer::from_text("Foo foo\r\nxfoo\n가foo").snapshot();
        let smart = crate::search::Opts::default();
        assert_eq!(find_all(&t, "foo", smart), vec![0..3, 4..7, 10..13, 17..20]);
        assert_eq!(find_all(&t, "Foo", smart), vec![0..3]);
        assert!(find_all(&t, "", smart).is_empty());
        assert!(find_all(&t, "zz", smart).is_empty());
        let case = crate::search::Opts {
            case: true,
            whole: false,
        };
        assert_eq!(find_all(&t, "foo", case), vec![4..7, 10..13, 17..20]);
        let whole = crate::search::Opts {
            case: false,
            whole: true,
        };
        assert_eq!(
            find_all(&t, "foo", whole),
            vec![0..3, 4..7],
            "xfoo and 가foo touch a word"
        );
    }

    #[test]
    fn next_and_prev_wrap() {
        let bar = Bar::default();
        let hits = vec![2..3, 5..6, 9..10];
        assert_eq!(bar.after(&hits, 0), Some(0));
        assert_eq!(bar.after(&hits, 5), Some(1));
        assert_eq!(bar.after(&hits, 10), Some(0));
        assert_eq!(bar.before(&hits, 5), Some(0));
        assert_eq!(bar.before(&hits, 2), Some(2));
        assert_eq!(bar.after(&[], 0), None);
    }

    #[test]
    fn goto_text() {
        assert_eq!(parse_goto("12"), Some((11, 0)));
        assert_eq!(parse_goto(" 3:7 "), Some((2, 6)));
        assert_eq!(parse_goto("0"), Some((0, 0)));
        assert_eq!(parse_goto("x"), None);
        assert_eq!(parse_goto("4:"), Some((3, 0)));
    }
}
