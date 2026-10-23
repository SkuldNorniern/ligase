//! carets, selections and how keys move them. plain byte offsets into one
//! snapshot; nothing here draws or edits.

use std::ops::Range;

use resolvase::{Encoding, Point, TextSnapshot};

/// how far word moves look ahead; past this a "word" just ends.
const WORD_REACH: usize = 4096;

/// caret `head`, other end `anchor`. empty when both match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub const fn caret(at: usize) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }

    pub fn range(self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    PageUp(usize),
    PageDown(usize),
    /// first non-blank, then column 0.
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
}

/// moves the caret. `extend` keeps the anchor. `goal` is the column up and
/// down aim for, kept across vertical moves and cleared by the rest.
pub fn motion(
    text: &TextSnapshot,
    sel: Selection,
    motion: Motion,
    extend: bool,
    goal: &mut Option<usize>,
) -> Selection {
    let head = sel.head;
    // left/right on a selection without shift lands on its edge
    if !extend && !sel.is_empty() {
        match motion {
            Motion::Left => return Selection::caret(sel.range().start),
            Motion::Right => return Selection::caret(sel.range().end),
            _ => {}
        }
    }
    let vertical = matches!(
        motion,
        Motion::Up | Motion::Down | Motion::PageUp(_) | Motion::PageDown(_)
    );
    if !vertical {
        *goal = None;
    }
    let to = match motion {
        Motion::Left => text.prev_grapheme(head).ok().flatten().unwrap_or(0),
        Motion::Right => text
            .next_grapheme(head)
            .ok()
            .flatten()
            .unwrap_or(text.len()),
        Motion::WordLeft => word_left(text, head),
        Motion::WordRight => word_right(text, head),
        Motion::Up => vertical_move(text, head, -1, goal),
        Motion::Down => vertical_move(text, head, 1, goal),
        Motion::PageUp(n) => vertical_move(text, head, -(n.max(1) as isize), goal),
        Motion::PageDown(n) => vertical_move(text, head, n.max(1) as isize, goal),
        Motion::LineStart => {
            let line = text.line_of(head).unwrap_or(0);
            let start = text.line_start(line).unwrap_or(0);
            let first = first_non_blank(text, line);
            if head == first { start } else { first }
        }
        Motion::LineEnd => line_end(text, text.line_of(head).unwrap_or(0)),
        Motion::DocStart => 0,
        Motion::DocEnd => text.len(),
    };
    Selection {
        anchor: if extend { sel.anchor } else { to },
        head: to,
    }
}

/// end of a line's text, before its `\n` and any `\r` ahead of it.
pub fn line_end(text: &TextSnapshot, line: usize) -> usize {
    let Ok(range) = text.line_range(line) else {
        return text.len();
    };
    let cr = range.end > range.start
        && text
            .slice(range.end - 1..range.end)
            .is_ok_and(|s| s == "\r");
    if cr { range.end - 1 } else { range.end }
}

fn first_non_blank(text: &TextSnapshot, line: usize) -> usize {
    let start = text.line_start(line).unwrap_or(0);
    let end = line_end(text, line);
    let blank: usize = text
        .chunks(start..end)
        .into_iter()
        .flatten()
        .flat_map(str::chars)
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(char::len_utf8)
        .sum();
    start + blank
}

/// leading spaces and tabs of the line holding `at`, up to `at`.
pub fn indent_before(text: &TextSnapshot, at: usize) -> String {
    let line = text.line_of(at).unwrap_or(0);
    let start = text.line_start(line).unwrap_or(0);
    let end = first_non_blank(text, line).min(at);
    text.slice(start..end).unwrap_or_default()
}

fn vertical_move(text: &TextSnapshot, head: usize, by: isize, goal: &mut Option<usize>) -> usize {
    let Ok(here) = text.point(head, Encoding::Utf32) else {
        return head;
    };
    let column = *goal.get_or_insert(here.column);
    let last = text.line_count() - 1;
    let line = here.line.checked_add_signed(by);
    let line = match line {
        None => return 0,
        Some(line) if line > last => return text.len(),
        Some(line) => line,
    };
    let end = line_end(text, line);
    let width = text.point(end, Encoding::Utf32).map_or(0, |p| p.column);
    text.offset(Point::new(line, column.min(width)), Encoding::Utf32)
        .unwrap_or(end)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Mark,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Mark
    }
}

/// past spaces, then past one run of word or mark chars.
fn word_right(text: &TextSnapshot, head: usize) -> usize {
    let mut end = (head + WORD_REACH).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let Ok(ahead) = text.slice(head..end) else {
        return head;
    };
    let mut chars = ahead.char_indices().peekable();
    while chars.next_if(|(_, c)| class(*c) == Class::Space).is_some() {}
    if let Some(&(_, first)) = chars.peek() {
        let kind = class(first);
        while chars.next_if(|(_, c)| class(*c) == kind).is_some() {}
    }
    head + chars.peek().map_or(ahead.len(), |(i, _)| *i)
}

fn word_left(text: &TextSnapshot, head: usize) -> usize {
    let mut start = head.saturating_sub(WORD_REACH);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let Ok(behind) = text.slice(start..head) else {
        return head;
    };
    let mut chars = behind.char_indices().rev().peekable();
    while chars.next_if(|(_, c)| class(*c) == Class::Space).is_some() {}
    if let Some(&(_, last)) = chars.peek() {
        let kind = class(last);
        while chars.next_if(|(_, c)| class(*c) == kind).is_some() {}
    }
    start + chars.peek().map_or(0, |(i, c)| i + c.len_utf8())
}

/// the word (or run of marks) around `at`, for double click.
pub fn word_at(text: &TextSnapshot, at: usize) -> Selection {
    let start = word_left(text, word_right(text, at));
    let start = if start > at {
        word_left(text, at)
    } else {
        start
    };
    Selection {
        anchor: start,
        head: word_right(text, start),
    }
}

/// the whole line around `at` with its line break, for triple click.
pub fn line_at(text: &TextSnapshot, at: usize) -> Selection {
    let line = text.line_of(at).unwrap_or(0);
    let start = text.line_start(line).unwrap_or(0);
    let end = if line + 1 < text.line_count() {
        text.line_start(line + 1).unwrap_or(text.len())
    } else {
        text.len()
    };
    Selection {
        anchor: start,
        head: end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use resolvase::TextBuffer;

    fn snap(text: &str) -> TextSnapshot {
        TextBuffer::from_text(text).snapshot()
    }

    fn go(text: &TextSnapshot, at: usize, m: Motion) -> usize {
        let mut goal = None;
        motion(text, Selection::caret(at), m, false, &mut goal).head
    }

    #[test]
    fn left_right_step_graphemes_and_crlf() {
        let t = snap("a가\r\nb");
        assert_eq!(go(&t, 1, Motion::Right), 4);
        assert_eq!(go(&t, 4, Motion::Right), 6, "crlf is one step");
        assert_eq!(go(&t, 6, Motion::Left), 4);
        assert_eq!(go(&t, 0, Motion::Left), 0);
        assert_eq!(go(&t, 7, Motion::Right), 7);
    }

    #[test]
    fn selection_collapses_to_its_edge() {
        let t = snap("abcdef");
        let mut goal = None;
        let sel = Selection { anchor: 1, head: 4 };
        assert_eq!(
            motion(&t, sel, Motion::Left, false, &mut goal),
            Selection::caret(1)
        );
        assert_eq!(
            motion(&t, sel, Motion::Right, false, &mut goal),
            Selection::caret(4)
        );
        let grown = motion(&t, sel, Motion::Right, true, &mut goal);
        assert_eq!(grown, Selection { anchor: 1, head: 5 });
    }

    #[test]
    fn up_down_keep_goal_column() {
        let t = snap("long line\nab\nanother");
        let mut goal = None;
        let mut sel = Selection::caret(7);
        sel = motion(&t, sel, Motion::Down, false, &mut goal);
        assert_eq!(sel.head, 12, "clamped to end of 'ab'");
        sel = motion(&t, sel, Motion::Down, false, &mut goal);
        assert_eq!(sel.head, 13 + 7, "goal 7 comes back");
        sel = motion(&t, sel, Motion::Down, false, &mut goal);
        assert_eq!(sel.head, t.len());
        sel = motion(&t, Selection::caret(3), Motion::Up, false, &mut goal);
        assert_eq!(sel.head, 0);
    }

    #[test]
    fn crlf_line_end_stops_before_cr() {
        let t = snap("ab\r\ncd");
        assert_eq!(line_end(&t, 0), 2);
        assert_eq!(go(&t, 0, Motion::LineEnd), 2);
        let mut goal = None;
        let down = motion(&t, Selection::caret(2), Motion::Up, false, &mut goal);
        assert_eq!(down.head, 0);
    }

    #[test]
    fn home_toggles_first_non_blank() {
        let t = snap("    let x;");
        assert_eq!(go(&t, 8, Motion::LineStart), 4);
        assert_eq!(go(&t, 4, Motion::LineStart), 0);
        assert_eq!(indent_before(&t, 8), "    ");
        assert_eq!(indent_before(&t, 2), "  ");
    }

    #[test]
    fn words_skip_spaces_then_one_run() {
        let t = snap("foo_bar  (x) 가나");
        assert_eq!(go(&t, 0, Motion::WordRight), 7);
        assert_eq!(go(&t, 7, Motion::WordRight), 10);
        assert_eq!(go(&t, 10, Motion::WordRight), 11);
        assert_eq!(go(&t, t.len(), Motion::WordLeft), 13);
        assert_eq!(go(&t, 9, Motion::WordLeft), 0);
        assert_eq!(word_at(&t, 2), Selection { anchor: 0, head: 7 });
        assert_eq!(line_at(&snap("a\nb"), 0), Selection { anchor: 0, head: 2 });
    }
}
