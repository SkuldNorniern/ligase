//! what each drawn row shows: a whole line, or one piece of a line when it
//! wraps. folded lines take no rows. without wrapping this is the fold map
//! alone; with it, a count of pieces per line kept per view.

use std::ops::Range;

use resolvase::TextSnapshot;

use super::cursor;
use super::fold::{self, Hidden};

/// files longer than this don't wrap; counting pieces is a pass over all of
/// the text whenever it changes.
pub const WRAP_MAX: usize = 4 * 1024 * 1024;

/// columns a char takes: a tab to the next stop, wide east asian chars 2.
fn width(c: char, col: usize, tab: usize) -> usize {
    match c {
        '\t' => tab - col % tab,
        c if is_wide(c) => 2,
        _ => 1,
    }
}

fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF
        | 0x20000..=0x3FFFD)
}

/// the pieces `line` wraps into at `cols` columns, as byte ranges in it.
/// a break goes after the last space that fits, else where the width runs
/// out. never empty: an empty line is one empty piece.
pub fn pieces(line: &str, cols: usize, tab: usize) -> Vec<Range<usize>> {
    let cols = cols.max(8);
    let tab = tab.max(1);
    let mut out = Vec::new();
    let mut start = 0;
    let mut col = 0;
    // just after the last space in this piece
    let mut soft: Option<usize> = None;
    for (i, c) in line.char_indices() {
        let w = width(c, col, tab);
        if col + w > cols && i > start {
            let cut = soft.filter(|&s| s > start).unwrap_or(i);
            out.push(start..cut);
            start = cut;
            // what was after the break carries on into the next piece
            col = line[start..i]
                .chars()
                .fold(0, |col, c| col + width(c, col, tab));
            soft = None;
        }
        col += width(c, col, tab);
        if c == ' ' || c == '\t' {
            soft = Some(i + c.len_utf8());
        }
    }
    out.push(start..line.len());
    out
}

/// the column of byte `at` inside `piece` of `line`.
pub fn column_in(line: &str, piece: &Range<usize>, at: usize, tab: usize) -> usize {
    let end = at.clamp(piece.start, piece.end);
    line[piece.start..end]
        .chars()
        .fold(0, |col, c| col + width(c, col, tab.max(1)))
}

/// the byte in `piece` of `line` nearest column `col`; a piece that isn't
/// the last stops before its end, so the caret stays on it.
pub fn at_column(line: &str, piece: &Range<usize>, col: usize, tab: usize, last: bool) -> usize {
    let mut at = piece.start;
    let mut c_col = 0;
    for (i, c) in line[piece.start..piece.end].char_indices() {
        let w = width(c, c_col, tab.max(1));
        if c_col + w > col {
            return piece.start + i;
        }
        c_col += w;
        at = piece.start + i + c.len_utf8();
    }
    if !last && at == piece.end && at > piece.start {
        // the end of a wrapped piece is the next piece's start
        let back = line[piece.start..piece.end]
            .char_indices()
            .next_back()
            .map_or(piece.start, |(i, _)| piece.start + i);
        return back;
    }
    at
}

/// a line's text without its break.
fn line_text(text: &TextSnapshot, line: usize) -> (usize, String) {
    let start = text.line_start(line).unwrap_or(0);
    let end = cursor::line_end(text, line);
    (start, text.slice(start..end).unwrap_or_default())
}

/// the row map for one frame.
pub struct Rows {
    pub hidden: Hidden,
    /// with wrapping: the first row of each line, and one past the last.
    first: Option<Vec<usize>>,
    pub cols: usize,
    pub tab: usize,
}

/// the wrap counts a view keeps between frames, made again only when the
/// text, the width, the tab or the folds change.
#[derive(Clone, Debug, Default)]
pub struct WrapCache {
    key: Option<(resolvase::Revision, usize, usize, Vec<Range<usize>>)>,
    first: Vec<usize>,
}

impl Rows {
    /// rows for `text` with `folded` folds, wrapped at `cols` when given.
    pub fn new(
        text: &TextSnapshot,
        folded: &[Range<usize>],
        wrap: Option<usize>,
        tab: usize,
        cache: &mut WrapCache,
    ) -> Self {
        let hidden = fold::hidden(text, folded);
        let Some(cols) = wrap.filter(|_| text.len() <= WRAP_MAX) else {
            return Self {
                hidden,
                first: None,
                cols: 0,
                tab,
            };
        };
        let key = (text.revision(), cols, tab, folded.to_vec());
        if cache.key.as_ref() != Some(&key) {
            let lines = text.line_count();
            let mut first = Vec::with_capacity(lines + 1);
            let mut row = 0;
            for line in 0..lines {
                first.push(row);
                if fold::is_hidden(line, &hidden).is_none() {
                    let (_, body) = line_text(text, line);
                    row += pieces(&body, cols, tab).len();
                }
            }
            first.push(row);
            cache.first = first;
            cache.key = Some(key);
        }
        Self {
            hidden,
            first: Some(cache.first.clone()),
            cols,
            tab,
        }
    }

    pub fn wraps(&self) -> bool {
        self.first.is_some()
    }

    pub fn total(&self, lines: usize) -> usize {
        match &self.first {
            Some(first) => first.last().copied().unwrap_or(1),
            None => fold::rows(lines, &self.hidden),
        }
        .max(1)
    }

    /// the line on `row` and which piece of it.
    pub fn line_of_row(&self, row: usize) -> (usize, usize) {
        match &self.first {
            Some(first) => {
                let line = first.partition_point(|&r| r <= row).saturating_sub(1);
                let line = line.min(first.len().saturating_sub(2));
                (line, row.saturating_sub(first[line]))
            }
            None => (fold::line_of_row(row, &self.hidden), 0),
        }
    }

    /// the pieces of `line`: its whole span when not wrapping.
    pub fn pieces_of(&self, text: &TextSnapshot, line: usize) -> Vec<Range<usize>> {
        let (start, body) = line_text(text, line);
        if !self.wraps() {
            return std::iter::once(start..start + body.len()).collect();
        }
        pieces(&body, self.cols, self.tab)
            .into_iter()
            .map(|r| start + r.start..start + r.end)
            .collect()
    }

    /// the row `at` is drawn on, and the piece it is in.
    pub fn row_of(&self, text: &TextSnapshot, at: usize) -> (usize, Range<usize>) {
        let line = text.line_of(at).unwrap_or(0);
        let pieces = self.pieces_of(text, line);
        let k = pieces.iter().rposition(|p| p.start <= at).unwrap_or(0);
        let row = match &self.first {
            Some(first) => first.get(line).copied().unwrap_or(0) + k,
            None => fold::row_of(line, &self.hidden),
        };
        (row, pieces[k].clone())
    }
}

/// up or down one drawn row through wrapped lines, keeping `goal` columns.
/// none when there is no row that way. folded lines are left to the caller.
pub fn step(
    text: &TextSnapshot,
    head: usize,
    up: bool,
    cols: usize,
    tab: usize,
    goal: &mut Option<usize>,
) -> Option<usize> {
    let line = text.line_of(head).ok()?;
    let (start, body) = line_text(text, line);
    let here = pieces(&body, cols, tab);
    let k = here
        .iter()
        .rposition(|p| p.start + start <= head)
        .unwrap_or(0);
    let col = *goal.get_or_insert_with(|| column_in(&body, &here[k], head - start, tab));
    let (line, k) = if up {
        if k > 0 {
            (line, k - 1)
        } else {
            let prev = line.checked_sub(1)?;
            let (_, b) = line_text(text, prev);
            (prev, pieces(&b, cols, tab).len() - 1)
        }
    } else if k + 1 < here.len() {
        (line, k + 1)
    } else {
        if line + 1 >= text.line_count() {
            return None;
        }
        (line + 1, 0)
    };
    let (start, body) = line_text(text, line);
    let all = pieces(&body, cols, tab);
    let last = k + 1 == all.len();
    Some(start + at_column(&body, &all[k], col, tab, last))
}

#[cfg(test)]
mod tests {
    use super::*;
    use resolvase::TextBuffer;

    #[test]
    fn breaks_after_spaces_else_where_width_ends() {
        assert_eq!(pieces("aaaa bbbb cccc", 10, 4), vec![0..10, 10..14]);
        assert_eq!(pieces("abcdefghijkl", 8, 4), vec![0..8, 8..12]);
        assert_eq!(pieces("", 10, 4), vec![0..0]);
        // wide chars take two columns: 4 hangul fill 8
        let ko = "가나다라마";
        assert_eq!(pieces(ko, 8, 4), vec![0..12, 12..15]);
    }

    #[test]
    fn columns_and_back() {
        let line = "abcdefghij";
        let all = pieces(line, 8, 4);
        assert_eq!(column_in(line, &all[1], 9, 4), 1);
        assert_eq!(at_column(line, &all[1], 1, 4, true), 9);
        assert_eq!(
            at_column(line, &all[0], 50, 4, false),
            7,
            "stays on the piece"
        );
    }

    #[test]
    fn rows_count_pieces_and_skip_folds() {
        let t = TextBuffer::from_text("aaaa bbbb cccc\nx\nyyyy zzzz ww").snapshot();
        let mut cache = WrapCache::default();
        let rows = Rows::new(&t, &[], Some(10), 4, &mut cache);
        assert_eq!(rows.total(3), 5);
        assert_eq!(rows.line_of_row(1), (0, 1));
        assert_eq!(rows.line_of_row(2), (1, 0));
        assert_eq!(rows.line_of_row(4), (2, 1));
        let (row, piece) = rows.row_of(&t, 12);
        assert_eq!((row, piece), (1, 10..14));
        let plain = Rows::new(&t, &[], None, 4, &mut cache);
        assert_eq!(plain.total(3), 3);
    }

    #[test]
    fn up_and_down_walk_drawn_rows() {
        let t = TextBuffer::from_text("aaaa bbbb cccc\nxy").snapshot();
        let mut goal = None;
        let down = step(&t, 2, false, 10, 4, &mut goal).unwrap();
        assert_eq!(down, 12, "second piece, same column");
        let down = step(&t, down, false, 10, 4, &mut goal).unwrap();
        assert_eq!(down, 17, "next line, clamped to its end");
        assert!(step(&t, down, false, 10, 4, &mut goal).is_none());
        let mut goal = None;
        assert_eq!(step(&t, 16, true, 10, 4, &mut goal), Some(11));
    }
}
