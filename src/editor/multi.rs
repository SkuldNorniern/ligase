//! more than one caret. the view keeps its main selection in `sel` and the
//! others in `extra`; everything here keeps them sorted and apart.

use resolvase::TextSnapshot;

use super::cursor::{self, Motion, Selection};
use crate::document::View;

/// most carets a view holds; past this new ones are not added.
pub const MAX_CARETS: usize = 10_000;

pub fn shifted(sel: Selection, by: isize) -> Selection {
    Selection {
        anchor: sel.anchor.saturating_add_signed(by),
        head: sel.head.saturating_add_signed(by),
    }
}

/// every selection of the view in text order, and which one is the main.
pub fn all(view: &View) -> (Vec<Selection>, usize) {
    let mut sels = Vec::with_capacity(view.extra.len() + 1);
    sels.push(view.sel);
    sels.extend(&view.extra);
    sels.sort_by_key(|s| (s.range().start, s.range().end));
    let primary = sels.iter().position(|s| *s == view.sel).unwrap_or(0);
    (sels, primary)
}

/// puts `sels` (in text order) back on the view, `primary` as the main.
pub fn set_all(view: &mut View, mut sels: Vec<Selection>, primary: usize) {
    if sels.is_empty() {
        return;
    }
    let main = sels.remove(primary.min(sels.len() - 1));
    view.sel = main;
    view.extra = sels;
    merge(view);
}

/// joins selections that touch or overlap; the main one keeps its place.
pub fn merge(view: &mut View) {
    if view.extra.is_empty() {
        return;
    }
    let (sels, primary) = all(view);
    let mut out: Vec<(Selection, bool)> = Vec::with_capacity(sels.len());
    for (i, sel) in sels.into_iter().enumerate() {
        let main = i == primary;
        if let Some((last, was_main)) = out.last_mut() {
            let (a, b) = (last.range(), sel.range());
            let same_caret = a.is_empty() && b.is_empty() && a.start == b.start;
            if same_caret || b.start < a.end || (b.start == a.end && !a.is_empty() && !b.is_empty())
            {
                let start = a.start.min(b.start);
                let end = a.end.max(b.end);
                // the joined one points the way the later one did
                let forward = sel.anchor <= sel.head;
                *last = if forward {
                    Selection {
                        anchor: start,
                        head: end,
                    }
                } else {
                    Selection {
                        anchor: end,
                        head: start,
                    }
                };
                *was_main |= main;
                continue;
            }
        }
        out.push((sel, main));
    }
    let primary = out.iter().position(|(_, m)| *m).unwrap_or(0);
    let (main, _) = out.remove(primary);
    view.sel = main;
    view.extra = out.into_iter().map(|(s, _)| s).collect();
}

/// next place `needle` sits after `from`, wrapping round, skipping places
/// already taken. bounded: looks through the text once.
fn next_match(
    text: &TextSnapshot,
    needle: &str,
    from: usize,
    taken: &[Selection],
) -> Option<std::ops::Range<usize>> {
    let all = text.slice(0..text.len()).ok()?;
    let found = |at: usize| {
        all[at..]
            .match_indices(needle)
            .map(move |(i, _)| at + i..at + i + needle.len())
    };
    let mut from = from.min(all.len());
    while !all.is_char_boundary(from) {
        from += 1;
    }
    found(from)
        .chain(found(0).take_while(move |r| r.start < from))
        .find(|r| !taken.iter().any(|s| s.range() == *r))
}

/// ctrl+d: a caret with nothing selected takes its word; after that each
/// press adds the next copy of the selected text.
pub fn add_next(text: &TextSnapshot, view: &mut View) -> bool {
    if view.sel.is_empty() && view.extra.is_empty() {
        let word = cursor::word_at(text, view.sel.head);
        if word.is_empty() {
            return false;
        }
        view.sel = word;
        return true;
    }
    if view.extra.len() >= MAX_CARETS {
        return false;
    }
    let Ok(needle) = text.slice(view.sel.range()) else {
        return false;
    };
    if needle.is_empty() {
        return false;
    }
    let (sels, _) = all(view);
    let last = sels.iter().map(|s| s.range().end).max().unwrap_or(0);
    let from = view.sel.range().end.max(last);
    let Some(next) = next_match(text, &needle, from, &sels) else {
        return false;
    };
    let old = view.sel;
    view.extra.push(old);
    view.sel = Selection {
        anchor: next.start,
        head: next.end,
    };
    merge(view);
    true
}

/// ctrl+shift+l: every copy of the selected text, or of the word under
/// the caret, gets a selection.
pub fn select_all_matches(text: &TextSnapshot, view: &mut View) -> bool {
    if view.sel.is_empty() {
        let word = cursor::word_at(text, view.sel.head);
        if word.is_empty() {
            return false;
        }
        view.sel = word;
    }
    let Ok(needle) = text.slice(view.sel.range()) else {
        return false;
    };
    let Ok(all) = text.slice(0..text.len()) else {
        return false;
    };
    let main = view.sel.range();
    view.extra = all
        .match_indices(needle.as_str())
        .map(|(i, _)| i..i + needle.len())
        .filter(|r| *r != main)
        .take(MAX_CARETS)
        .map(|r| Selection {
            anchor: r.start,
            head: r.end,
        })
        .collect();
    merge(view);
    true
}

/// ctrl+alt+up/down: a caret on the line above or below the topmost or
/// bottom one, at the main caret's column.
pub fn add_vertical(text: &TextSnapshot, view: &mut View, up: bool) -> bool {
    if view.extra.len() >= MAX_CARETS {
        return false;
    }
    let (sels, _) = all(view);
    let edge = if up { sels[0] } else { sels[sels.len() - 1] };
    let mut goal = view.goal.or_else(|| {
        text.point(view.sel.head, resolvase::Encoding::Utf32)
            .ok()
            .map(|p| p.column)
    });
    let motion = if up { Motion::Up } else { Motion::Down };
    let moved = cursor::motion(text, Selection::caret(edge.head), motion, false, &mut goal);
    if text.line_of(moved.head).ok() == text.line_of(edge.head).ok() {
        return false;
    }
    view.goal = goal;
    view.extra.push(view.sel);
    view.sel = moved;
    merge(view);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Documents;
    use resolvase::TextBuffer;
    use std::path::Path;

    fn view() -> View {
        let mut docs = Documents::default();
        let (doc, _) = docs.open(Path::new("t.rs"));
        let id = docs.add_view(doc);
        docs.view(id).unwrap().clone()
    }

    fn snap(t: &str) -> TextSnapshot {
        TextBuffer::from_text(t).snapshot()
    }

    fn sel(a: usize, h: usize) -> Selection {
        Selection { anchor: a, head: h }
    }

    #[test]
    fn merge_joins_overlaps_and_keeps_the_main() {
        let mut v = view();
        v.sel = sel(4, 6);
        v.extra = vec![
            sel(0, 2),
            sel(5, 9),
            Selection::caret(12),
            Selection::caret(12),
        ];
        merge(&mut v);
        assert_eq!(v.sel, sel(4, 9));
        assert_eq!(v.extra, vec![sel(0, 2), Selection::caret(12)]);
    }

    #[test]
    fn ctrl_d_takes_word_then_next_copies() {
        let t = snap("ab x ab y ab");
        let mut v = view();
        v.sel = Selection::caret(1);
        assert!(add_next(&t, &mut v));
        assert_eq!(v.sel, sel(0, 2));
        assert!(add_next(&t, &mut v));
        assert_eq!(v.sel, sel(5, 7));
        assert!(add_next(&t, &mut v));
        assert_eq!(v.sel, sel(10, 12));
        assert!(!add_next(&t, &mut v), "all taken");
        assert_eq!(v.extra.len(), 2);
    }

    #[test]
    fn select_all_and_add_below() {
        let t = snap("ab\nab\nab");
        let mut v = view();
        v.sel = Selection::caret(0);
        assert!(select_all_matches(&t, &mut v));
        assert_eq!(v.extra.len(), 2);
        let mut v = view();
        v.sel = Selection::caret(1);
        assert!(add_vertical(&t, &mut v, false));
        assert!(add_vertical(&t, &mut v, false));
        assert_eq!(v.sel, Selection::caret(7));
        assert!(!add_vertical(&t, &mut v, false), "no line below");
        assert_eq!(all(&v).0.len(), 3);
    }
}
